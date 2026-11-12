"""Exposition kernel host: a thin managed helper around jupyter_client.

The Rust research supervisor launches this file with a selected Python
interpreter (executable + argument array, deliberately constructed
environment, working directory = the trusted workspace root) and talks to it
over JSON lines:

* stdin  -> one request object per line: ``{"id": <int>, "cmd": <name>, ...}``
* stdout -> one message object per line, either a response
  ``{"type": "response", "id": <int>, "ok": true, "result": {...}}`` /
  ``{"type": "response", "id": <int>, "ok": false, "error": {"code", "message"}}``
  or an event ``{"type": "event", "event": <name>, ...}``.

Nothing else is ever written to stdout: Python-level and C-level writes of this
process and of the kernel go to stderr, which the supervisor keeps as a bounded
diagnostic log.

Protocol version 1. Commands:

``handshake {protocol}``
    Must be first. Replies with helper/Python/jupyter_client versions and key
    package versions (read from package metadata; nothing is imported).
``kernelspecs``
    Installed Jupyter kernelspecs visible to this interpreter (for R/Julia).
``start {kernel: {kind: "python"} | {kind: "kernelspec", name}, cwd}``
    Start one kernel. Python kernels use this interpreter's ipykernel.
``execute {run_id, code, output_dir}``
    Queue code; outputs stream as events tagged with ``run_id``. Rich outputs
    (PNG/JPEG/GIF/SVG images, HTML) are written as files under ``output_dir``;
    HTML is saved as inert text and never executed. The first HTML table of an
    output is also converted to a CSV grid.
``interrupt`` / ``restart`` / ``status`` / ``ping`` / ``shutdown``

Events: ``heartbeat``, ``kernel_status``, ``run_started``, ``stream``,
``display``, ``clear_output``, ``error``, ``run_finished``, ``kernel_died``,
``log``.

This file is also the Python kernel entry point (``--ipykernel -f <file>``):
it starts ipykernel with interrupts that wake blocking sleeps on Windows.

Dependencies: the standard library and jupyter_client (plus ipykernel for
Python kernels). The helper never installs anything.
"""

from __future__ import annotations

import base64
import binascii
import csv
import hashlib
import html.parser
import io
import json
import os
import queue
import re
import sys
import threading
import time
import traceback

PROTOCOL_VERSION = 1
HELPER_VERSION = "0.1.0"

#: Largest stream text carried by one event (characters).
STREAM_CHUNK = 16 * 1024
#: Stream text forwarded per run before further text is dropped (characters).
STREAM_RUN_LIMIT = 8 * 1024 * 1024
#: How long stream text is coalesced before it is flushed (seconds).
STREAM_FLUSH_INTERVAL = 0.05
#: Rich output files written per run before further files are dropped.
FILES_PER_RUN_LIMIT = 200
#: Largest single rich output written to disk (bytes).
FILE_SIZE_LIMIT = 64 * 1024 * 1024
#: Heartbeat period (seconds).
HEARTBEAT_INTERVAL = 2.0
#: Packages whose versions are reported for run provenance.
KEY_PACKAGES = (
    "jupyter_client",
    "ipykernel",
    "pyzmq",
    "numpy",
    "pandas",
    "matplotlib",
    "scipy",
    "sympy",
    "polars",
)

IMAGE_TYPES = {
    "image/png": ("png", True),
    "image/jpeg": ("jpg", True),
    "image/gif": ("gif", True),
    "image/svg+xml": ("svg", False),
}

ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")


def strip_ansi(text: str) -> str:
    return ANSI_ESCAPE.sub("", text)


def package_version(name: str) -> str | None:
    try:
        from importlib import metadata

        return metadata.version(name)
    except Exception:
        return None


class ProtocolError(Exception):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


# --------------------------------------------------------------------------
# Output channel


class Emitter:
    """Writes protocol lines to the reserved stdout stream (thread-safe)."""

    def __init__(self, stream: io.TextIOBase) -> None:
        self._stream = stream
        self._lock = threading.Lock()
        self.closed = False

    def send(self, message: dict) -> None:
        line = json.dumps(message, ensure_ascii=False, separators=(",", ":"))
        with self._lock:
            if self.closed:
                return
            try:
                self._stream.write(line + "\n")
                self._stream.flush()
            except (OSError, ValueError):
                # The supervisor is gone; nothing more can be reported.
                self.closed = True

    def event(self, event_name: str, /, **fields: object) -> None:
        self.send({"type": "event", "event": event_name, **fields})


def reserve_protocol_stdout() -> io.TextIOBase:
    """Keep the original stdout for protocol lines; route everything else to stderr."""
    protocol_fd = os.dup(sys.stdout.fileno())
    stream = os.fdopen(protocol_fd, "w", encoding="utf-8", newline="\n", buffering=1)
    sys.stdout.flush()
    try:
        os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    except OSError:
        pass
    sys.stdout = sys.stderr
    return stream


# --------------------------------------------------------------------------
# HTML table extraction (inert parsing; nothing is rendered or executed)


class _TableParser(html.parser.HTMLParser):
    """Collects the first top-level <table> as rows of text cells."""

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.depth = 0
        self.done = False
        self.rows: list[list[dict]] = []
        self.section: str | None = None
        self.cell: dict | None = None
        self.skip = 0  # inside <script>/<style>

    def handle_starttag(self, tag: str, attrs: list) -> None:
        if self.done:
            return
        if tag in ("script", "style"):
            self.skip += 1
            return
        if tag == "table":
            self.depth += 1
            return
        if self.depth != 1:
            return
        if tag in ("thead", "tbody", "tfoot", "tr", "td", "th"):
            self._close_cell()  # end tags are optional in HTML
        if tag in ("thead", "tbody", "tfoot"):
            self.section = tag
        elif tag == "tr":
            self.rows.append([{"section": self.section, "marker": True}])
        elif tag in ("td", "th") and self.rows:
            span = dict(attrs)
            self.cell = {
                "text": [],
                "header": tag == "th",
                "colspan": _span(span.get("colspan")),
                "rowspan": _span(span.get("rowspan")),
            }
        elif tag == "br" and self.cell is not None:
            self.cell["text"].append(" ")

    def handle_endtag(self, tag: str) -> None:
        if self.done:
            return
        if tag in ("script", "style"):
            self.skip = max(0, self.skip - 1)
            return
        if tag == "table":
            if self.depth == 1:
                self._close_cell()
            self.depth -= 1
            if self.depth == 0:
                self.done = True
            return
        if self.depth != 1:
            return
        if tag in ("td", "th", "tr"):
            self._close_cell()
        elif tag in ("thead", "tbody", "tfoot"):
            self._close_cell()
            self.section = None

    def _close_cell(self) -> None:
        if self.cell is not None and self.rows:
            self.rows[-1].append(self.cell)
        self.cell = None

    def handle_data(self, data: str) -> None:
        if self.skip or self.cell is None or self.depth != 1:
            return
        self.cell["text"].append(data)


def _span(value: object) -> int:
    try:
        return max(1, min(int(str(value)), 1000))
    except (TypeError, ValueError):
        return 1


def html_table_grid(markup: str) -> tuple[list[list[str]], int] | None:
    """Return (rows, header_row_count) for the first table in ``markup``."""
    parser = _TableParser()
    try:
        parser.feed(markup)
        parser.close()
    except Exception:
        return None
    raw_rows = [r for r in parser.rows if len(r) > 1]
    if not raw_rows:
        return None
    grid: list[list[str | None]] = []
    header_rows = 0
    pending: dict[int, tuple[str, int]] = {}  # column -> (text, rows remaining)
    for raw in raw_rows:
        marker, cells = raw[0], raw[1:]
        row: list[str | None] = []
        col = 0

        def fill_pending() -> None:
            nonlocal col
            while col in pending:
                text, remaining = pending[col]
                row.append(text)
                if remaining <= 1:
                    del pending[col]
                else:
                    pending[col] = (text, remaining - 1)
                col += 1

        for cell in cells:
            fill_pending()
            text = " ".join("".join(cell["text"]).split())
            for _ in range(cell["colspan"]):
                row.append(text)
                if cell["rowspan"] > 1:
                    pending[col] = (text, cell["rowspan"] - 1)
                col += 1
        fill_pending()
        section = marker["section"]
        is_header = section == "thead" or (section is None and bool(cells) and all(c["header"] for c in cells))
        if is_header and header_rows == len(grid):
            header_rows += 1
        grid.append(row)
    width = max(len(r) for r in grid)
    rows = [[c or "" for c in r] + [""] * (width - len(r)) for r in grid]
    if header_rows > 1:
        merged = []
        for column in range(width):
            parts = []
            for r in rows[:header_rows]:
                part = r[column]
                if part and (not parts or parts[-1] != part):
                    parts.append(part)
            merged.append(" ".join(parts))
        rows = [merged] + rows[header_rows:]
        header_rows = 1
    return rows, header_rows


def grid_to_csv(rows: list[list[str]]) -> str:
    buffer = io.StringIO()
    writer = csv.writer(buffer, lineterminator="\n")
    writer.writerows(rows)
    return buffer.getvalue()


# --------------------------------------------------------------------------
# Runs


class Run:
    def __init__(self, run_id: str, msg_id: str, output_dir: str) -> None:
        self.run_id = run_id
        self.msg_id = msg_id
        self.output_dir = output_dir
        self.reply: dict | None = None
        self.idle_seen = False
        self.started = False
        self.files_written = 0
        self.files_dropped = 0
        self.stream_chars = 0
        self.stream_dropped = 0
        self.buffers: dict[str, list[str]] = {}
        self.buffered_since: float | None = None
        self.output_seq = 0

    @property
    def complete(self) -> bool:
        return self.reply is not None and self.idle_seen


class KernelHost:
    def __init__(self, emitter: Emitter, helper_path: str) -> None:
        self.out = emitter
        self.helper_path = helper_path
        self.km = None
        self.kc = None
        self.kernel_request: dict | None = None
        self.kernel_info: dict = {}
        self.cwd: str | None = None
        self.execution_state = "none"
        self.runs: dict[str, Run] = {}  # by msg_id
        self.shutting_down = False
        self.expect_exit = False
        self.handshaken = False
        self.transport_encryption = "none"
        self.contained = False

    # ---- dispatch -------------------------------------------------------

    def handle(self, request: dict) -> bool:
        """Handle one request. Returns False when the helper should exit."""
        req_id = request.get("id")
        cmd = request.get("cmd")
        try:
            if not isinstance(req_id, int):
                raise ProtocolError("protocol.invalidRequest", "request id must be an integer")
            if cmd != "handshake" and not self.handshaken:
                raise ProtocolError("protocol.handshakeRequired", "handshake must be the first request")
            handler = getattr(self, f"cmd_{cmd}", None) if isinstance(cmd, str) else None
            if handler is None:
                raise ProtocolError("protocol.unknownCommand", f"unknown command {cmd!r}")
            result = handler(request)
            self.out.send({"type": "response", "id": req_id, "ok": True, "result": result or {}})
        except ProtocolError as error:
            self.out.send(
                {"type": "response", "id": req_id, "ok": False, "error": {"code": error.code, "message": error.message}}
            )
        except Exception as error:  # report, keep serving
            self.out.event("log", level="error", message=traceback.format_exc(limit=8))
            self.out.send(
                {
                    "type": "response",
                    "id": req_id,
                    "ok": False,
                    "error": {"code": "helper.internal", "message": f"{type(error).__name__}: {error}"},
                }
            )
        return not (cmd == "shutdown" and self.shutting_down)

    # ---- commands -------------------------------------------------------

    def cmd_handshake(self, request: dict) -> dict:
        protocol = request.get("protocol")
        if protocol != PROTOCOL_VERSION:
            raise ProtocolError(
                "protocol.versionMismatch",
                f"supervisor speaks protocol {protocol!r}; this helper speaks {PROTOCOL_VERSION}",
            )
        self.handshaken = True
        return {
            "protocol": PROTOCOL_VERSION,
            "helperVersion": HELPER_VERSION,
            "python": {
                "version": ".".join(str(p) for p in sys.version_info[:3]),
                "implementation": sys.implementation.name,
                "executable": sys.executable,
            },
            "packages": {name: package_version(name) for name in KEY_PACKAGES},
            "pid": os.getpid(),
            "childContainment": "job" if self.contained else "none",
        }

    def cmd_ping(self, _request: dict) -> dict:
        return {"time": time.time()}

    def cmd_kernelspecs(self, _request: dict) -> dict:
        from jupyter_client.kernelspec import KernelSpecManager

        specs = []
        for name, entry in sorted(KernelSpecManager().get_all_specs().items()):
            spec = entry.get("spec", {})
            argv = spec.get("argv") or []
            specs.append(
                {
                    "name": name,
                    "displayName": spec.get("display_name", name),
                    "language": spec.get("language", ""),
                    "argv0": argv[0] if argv else None,
                    "resourceDir": entry.get("resource_dir"),
                }
            )
        return {"kernelspecs": specs}

    def cmd_start(self, request: dict) -> dict:
        if self.km is not None:
            raise ProtocolError("kernel.alreadyStarted", "a kernel is already running in this helper")
        kernel = request.get("kernel") or {"kind": "python"}
        cwd = request.get("cwd")
        if not isinstance(cwd, str) or not os.path.isabs(cwd) or not os.path.isdir(cwd):
            raise ProtocolError("kernel.invalidCwd", "cwd must be an existing absolute directory")
        self.kernel_request = kernel
        self.cwd = cwd
        return self._start_kernel(float(request.get("timeout", 60)))

    def cmd_execute(self, request: dict) -> dict:
        if self.kc is None or self.km is None:
            raise ProtocolError("kernel.notRunning", "no kernel is running")
        if not self.km.is_alive():
            raise ProtocolError("kernel.dead", "the kernel is not running; restart it")
        run_id = request.get("run_id")
        code = request.get("code")
        output_dir = request.get("output_dir")
        if not isinstance(run_id, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}", run_id):
            raise ProtocolError("run.invalidId", "run_id must be a short identifier")
        if not isinstance(code, str):
            raise ProtocolError("run.invalidCode", "code must be a string")
        if not isinstance(output_dir, str) or not os.path.isabs(output_dir):
            raise ProtocolError("run.invalidOutputDir", "output_dir must be an absolute path")
        if any(r.run_id == run_id for r in self.runs.values()):
            raise ProtocolError("run.duplicate", f"run {run_id} is already executing")
        msg_id = self.kc.execute(code, silent=False, store_history=True, allow_stdin=False, stop_on_error=True)
        self.runs[msg_id] = Run(run_id, msg_id, output_dir)
        return {"msgId": msg_id}

    def cmd_interrupt(self, _request: dict) -> dict:
        if self.km is None:
            raise ProtocolError("kernel.notRunning", "no kernel is running")
        if not self.km.is_alive():
            raise ProtocolError("kernel.dead", "the kernel is not running; restart it")
        self.km.interrupt_kernel()
        return {}

    def cmd_restart(self, request: dict) -> dict:
        if self.kernel_request is None:
            raise ProtocolError("kernel.notRunning", "no kernel has been started")
        self.out.event("kernel_status", state="restarting")
        self._finish_runs_on_loss("restarted")
        self._stop_kernel(now=True)
        return self._start_kernel(float(request.get("timeout", 60)))

    def cmd_status(self, _request: dict) -> dict:
        alive = bool(self.km is not None and self.km.is_alive())
        return {
            "kernelAlive": alive,
            "executionState": self.execution_state if alive else ("dead" if self.km is not None else "none"),
            "pid": self._kernel_pid(),
            "runs": [r.run_id for r in self.runs.values()],
        }

    def cmd_shutdown(self, _request: dict) -> dict:
        self.shutting_down = True
        self._finish_runs_on_loss("shutdown")
        self._stop_kernel(now=False)
        return {}

    # ---- kernel lifecycle -----------------------------------------------

    def _start_kernel(self, timeout: float) -> dict:
        from jupyter_client.kernelspec import KernelSpec, KernelSpecManager, NoSuchKernel
        from jupyter_client.manager import KernelManager

        kernel = self.kernel_request or {"kind": "python"}
        kind = kernel.get("kind")
        manager_kwargs: dict = {}
        if kind == "python":
            try:
                import ipykernel  # noqa: F401  (presence check only)
            except ImportError:
                raise ProtocolError(
                    "kernel.ipykernelMissing",
                    f"ipykernel is not installed for {sys.executable}; install it into this interpreter",
                ) from None
            metadata: dict = {}
            curve = self._curve_supported()
            if curve:
                metadata["supported_encryption"] = "curve"
            spec = KernelSpec(
                argv=[
                    sys.executable,
                    self.helper_path,
                    "--ipykernel",
                    "-f",
                    "{connection_file}",
                    "--HistoryManager.enabled=False",
                ],
                display_name=f"Python {sys.version_info[0]}.{sys.version_info[1]}",
                language="python",
                interrupt_mode="signal",
                metadata=metadata,
            )

            class _Specs(KernelSpecManager):
                def get_kernel_spec(self, kernel_name: str, *args: object, **kwargs: object) -> KernelSpec:
                    return spec

            manager_kwargs = {"kernel_spec_manager": _Specs(), "kernel_name": "exposition-python"}
            if curve:
                manager_kwargs["transport_encryption"] = "auto"
        elif kind == "kernelspec":
            name = kernel.get("name")
            if not isinstance(name, str) or not name:
                raise ProtocolError("kernel.invalidSpec", "kernelspec name is required")
            try:
                KernelSpecManager().get_kernel_spec(name)
            except NoSuchKernel:
                raise ProtocolError("kernel.noSuchKernel", f"no Jupyter kernelspec named {name!r}") from None
            manager_kwargs = {"kernel_name": name}
        else:
            raise ProtocolError("kernel.invalidSpec", f"unknown kernel kind {kind!r}")

        env = dict(os.environ)
        env.setdefault("PYDEVD_DISABLE_FILE_VALIDATION", "1")
        try:
            km = KernelManager(**manager_kwargs)
        except Exception:
            manager_kwargs.pop("transport_encryption", None)
            km = KernelManager(**manager_kwargs)
        self.transport_encryption = "curve" if manager_kwargs.get("transport_encryption") else "none"
        self.out.event("kernel_status", state="starting")
        try:
            km.start_kernel(
                cwd=self.cwd,
                env=env,
                stdin=__import__("subprocess").DEVNULL,
                stdout=sys.stderr.fileno(),
                stderr=sys.stderr.fileno(),
            )
        except Exception as error:
            raise ProtocolError("kernel.startFailed", f"kernel failed to start: {type(error).__name__}: {error}")
        kc = km.client()
        kc.start_channels()
        try:
            kc.wait_for_ready(timeout=timeout)
        except Exception as error:
            try:
                kc.stop_channels()
                km.shutdown_kernel(now=True)
            except Exception:
                pass
            raise ProtocolError(
                "kernel.startFailed", f"kernel did not become ready: {type(error).__name__}: {error}"
            ) from None
        self.km = km
        self.kc = kc
        self.expect_exit = False
        info = self._kernel_info()
        self.execution_state = "idle"
        self.out.event("kernel_status", state="idle")
        return {
            "kernel": info,
            "pid": self._kernel_pid(),
            "transportEncryption": self.transport_encryption,
            "interrupt": self._interrupt_mode(),
        }

    def _curve_supported(self) -> bool:
        try:
            import zmq
            from ipykernel.kernelspec import get_kernel_dict
            from jupyter_client.manager import KernelManager

            if not zmq.has("curve") or not KernelManager.class_traits().get("transport_encryption"):
                return False
            declared = (get_kernel_dict().get("metadata") or {}).get("supported_encryption")
            return declared == "curve" or (isinstance(declared, (list, tuple)) and "curve" in declared)
        except Exception:
            return False

    def _interrupt_mode(self) -> str:
        if self.km is None:
            return "none"
        mode = getattr(self.km.kernel_spec, "interrupt_mode", "signal")
        if mode == "message":
            return "message"
        if sys.platform == "win32":
            # jupyter_client signals an interrupt event that only ipykernel
            # watches; other kernels (e.g. IRkernel) ignore it on Windows.
            python = (self.kernel_request or {}).get("kind") == "python"
            return "event" if python else "unsupported"
        return "signal"

    def _kernel_info(self) -> dict:
        assert self.kc is not None
        reply = None
        try:
            msg_id = self.kc.kernel_info()
            deadline = time.time() + 10
            while time.time() < deadline:
                msg = self.kc.get_shell_msg(timeout=max(0.1, deadline - time.time()))
                if msg.get("parent_header", {}).get("msg_id") == msg_id:
                    reply = msg.get("content", {})
                    break
        except Exception:
            reply = None
        reply = reply or {}
        language_info = reply.get("language_info") or {}
        self.kernel_info = {
            "name": getattr(self.km, "kernel_name", ""),
            "language": language_info.get("name", ""),
            "languageVersion": language_info.get("version", ""),
            "implementation": reply.get("implementation", ""),
            "implementationVersion": reply.get("implementation_version", ""),
            "protocolVersion": reply.get("protocol_version", ""),
            "banner": (reply.get("banner") or "")[:2000],
        }
        return self.kernel_info

    def _kernel_pid(self) -> int | None:
        try:
            process = getattr(getattr(self.km, "provisioner", None), "process", None)
            return getattr(process, "pid", None)
        except Exception:
            return None

    def _stop_kernel(self, now: bool) -> None:
        self.expect_exit = True
        if self.kc is not None:
            try:
                self.kc.stop_channels()
            except Exception:
                pass
        if self.km is not None:
            try:
                if now and self.km.is_alive():
                    # Launcher processes (e.g. R.exe -> Rterm.exe) must not
                    # leave the real kernel behind: end the whole tree first.
                    kill_process_tree(self._kernel_pid())
                if self.km.is_alive():
                    self.km.shutdown_kernel(now=now)
                else:
                    self.km.cleanup_resources()
            except Exception:
                try:
                    self.km.shutdown_kernel(now=True)
                except Exception:
                    pass
        self.km = None
        self.kc = None
        self.execution_state = "none"

    def _finish_runs_on_loss(self, reason: str) -> None:
        for run in list(self.runs.values()):
            self._flush_streams(run)
            self.out.event(
                "run_finished",
                run_id=run.run_id,
                status="lost",
                reason=reason,
                executionCount=None,
                streamDropped=run.stream_dropped,
                filesDropped=run.files_dropped,
            )
        self.runs.clear()

    def check_kernel(self) -> None:
        if self.km is None or self.expect_exit:
            return
        try:
            alive = self.km.is_alive()
        except Exception:
            alive = False
        if alive:
            return
        exit_code = None
        try:
            process = getattr(self.km.provisioner, "process", None)
            exit_code = process.poll() if process is not None else None
        except Exception:
            pass
        lost = [r.run_id for r in self.runs.values()]
        self._finish_runs_on_loss("kernelDied")
        self.execution_state = "dead"
        self.out.event("kernel_died", exitCode=exit_code, runs=lost)
        self.expect_exit = True  # report once; restart creates a new kernel

    # ---- message pump ---------------------------------------------------

    def pump(self, timeout: float) -> None:
        """Wait up to ``timeout`` seconds for kernel messages and dispatch them."""
        if self.kc is None:
            time.sleep(timeout)
            return
        import zmq

        poller = zmq.Poller()
        iopub = self.kc.iopub_channel.socket
        shell = self.kc.shell_channel.socket
        if iopub is None or shell is None:
            time.sleep(timeout)
            return
        poller.register(iopub, zmq.POLLIN)
        poller.register(shell, zmq.POLLIN)
        try:
            ready = dict(poller.poll(int(timeout * 1000)))
        except zmq.ZMQError:
            ready = {}
        if iopub in ready:
            while self.kc.iopub_channel.msg_ready():
                self.on_iopub(self.kc.iopub_channel.get_msg(timeout=0))
        if shell in ready:
            while self.kc.shell_channel.msg_ready():
                self.on_shell(self.kc.shell_channel.get_msg(timeout=0))
        now = time.monotonic()
        for run in list(self.runs.values()):
            if run.buffered_since is not None and now - run.buffered_since >= STREAM_FLUSH_INTERVAL:
                self._flush_streams(run)
            self._maybe_finish(run)

    def on_shell(self, msg: dict) -> None:
        parent = msg.get("parent_header", {}).get("msg_id")
        run = self.runs.get(parent)
        if run is None or msg.get("msg_type") != "execute_reply":
            return
        run.reply = msg.get("content", {})
        self._maybe_finish(run)

    def on_iopub(self, msg: dict) -> None:
        msg_type = msg.get("msg_type")
        content = msg.get("content", {})
        parent = msg.get("parent_header", {}).get("msg_id")
        run = self.runs.get(parent)
        if msg_type == "status":
            state = content.get("execution_state", "")
            if state in ("busy", "idle", "starting"):
                if state != self.execution_state:
                    self.execution_state = state
                    self.out.event("kernel_status", state=state)
            if run is not None and state == "idle":
                run.idle_seen = True
                self._maybe_finish(run)
            return
        if run is None:
            return
        if msg_type == "execute_input":
            run.started = True
            self.out.event("run_started", run_id=run.run_id, executionCount=content.get("execution_count"))
        elif msg_type == "stream":
            self._on_stream(run, content.get("name", "stdout"), content.get("text", ""))
        elif msg_type in ("display_data", "execute_result", "update_display_data"):
            self._flush_streams(run)
            self._on_display(run, msg_type, content)
        elif msg_type == "error":
            self._flush_streams(run)
            self.out.event(
                "error",
                run_id=run.run_id,
                ename=str(content.get("ename", ""))[:500],
                evalue=strip_ansi(str(content.get("evalue", "")))[:8000],
                traceback=[strip_ansi(str(line))[:8000] for line in (content.get("traceback") or [])[:200]],
            )
        elif msg_type == "clear_output":
            self._flush_streams(run)
            self.out.event("clear_output", run_id=run.run_id, wait=bool(content.get("wait")))

    def _on_stream(self, run: Run, name: str, text: str) -> None:
        name = "stderr" if name == "stderr" else "stdout"
        remaining = STREAM_RUN_LIMIT - run.stream_chars
        if remaining <= 0:
            run.stream_dropped += len(text)
            return
        if len(text) > remaining:
            run.stream_dropped += len(text) - remaining
            text = text[:remaining]
        run.stream_chars += len(text)
        run.buffers.setdefault(name, []).append(text)
        if run.buffered_since is None:
            run.buffered_since = time.monotonic()
        if sum(len(t) for t in run.buffers[name]) >= STREAM_CHUNK:
            self._flush_streams(run)

    def _flush_streams(self, run: Run) -> None:
        for name, parts in list(run.buffers.items()):
            text = "".join(parts)
            for start in range(0, len(text), STREAM_CHUNK):
                self.out.event("stream", run_id=run.run_id, name=name, text=text[start : start + STREAM_CHUNK])
        run.buffers.clear()
        run.buffered_since = None

    def _on_display(self, run: Run, output_type: str, content: dict) -> None:
        data = content.get("data") or {}
        files = []
        for media_type, (extension, is_base64) in IMAGE_TYPES.items():
            if media_type in data:
                written = self._write_output(run, extension, data[media_type], is_base64, media_type, "image")
                if written:
                    files.append(written)
                break  # one image representation per output
        html_text = data.get("text/html")
        if isinstance(html_text, list):
            html_text = "".join(html_text)
        if isinstance(html_text, str) and html_text:
            written = self._write_output(run, "html", html_text, False, "text/html", "html")
            if written:
                files.append(written)
                if "<table" in html_text.lower():
                    grid = html_table_grid(html_text)
                    if grid is not None:
                        rows, header_rows = grid
                        csv_file = self._write_output(
                            run, "csv", grid_to_csv(rows), False, "text/csv", "table", seq=written["seq"]
                        )
                        if csv_file:
                            csv_file["headerRows"] = header_rows
                            csv_file["rows"] = len(rows)
                            csv_file["columns"] = len(rows[0]) if rows else 0
                            files.append(csv_file)
        text = data.get("text/plain")
        if isinstance(text, list):
            text = "".join(text)
        self.out.event(
            "display",
            run_id=run.run_id,
            outputType=output_type,
            executionCount=content.get("execution_count"),
            text=strip_ansi(text)[:STREAM_CHUNK] if isinstance(text, str) else None,
            files=files,
        )

    def _write_output(
        self,
        run: Run,
        extension: str,
        payload: object,
        is_base64: bool,
        media_type: str,
        role: str,
        seq: int | None = None,
    ) -> dict | None:
        if run.files_written >= FILES_PER_RUN_LIMIT:
            run.files_dropped += 1
            return None
        if isinstance(payload, list):
            payload = "".join(str(p) for p in payload)
        if not isinstance(payload, str):
            return None
        try:
            raw = base64.b64decode(payload) if is_base64 else payload.encode("utf-8")
        except (binascii.Error, ValueError):
            self.out.event("log", level="warning", message=f"undecodable {media_type} output dropped")
            return None
        if len(raw) > FILE_SIZE_LIMIT:
            run.files_dropped += 1
            self.out.event("log", level="warning", message=f"{media_type} output over size limit dropped")
            return None
        if seq is None:
            run.output_seq += 1
            seq = run.output_seq
        os.makedirs(run.output_dir, exist_ok=True)
        name = f"output-{seq}.{extension}"
        path = os.path.join(run.output_dir, name)
        temp = path + ".partial"
        with open(temp, "wb") as handle:
            handle.write(raw)
        os.replace(temp, path)
        run.files_written += 1
        return {
            "path": path,
            "name": name,
            "mediaType": media_type,
            "role": role,
            "size": len(raw),
            "sha256": hashlib.sha256(raw).hexdigest(),
            "seq": seq,
        }

    def _maybe_finish(self, run: Run) -> None:
        if not run.complete or run.msg_id not in self.runs:
            return
        self._flush_streams(run)
        reply = run.reply or {}
        status = reply.get("status", "error")
        self.out.event(
            "run_finished",
            run_id=run.run_id,
            status=status if status in ("ok", "error", "aborted") else "error",
            executionCount=reply.get("execution_count"),
            streamDropped=run.stream_dropped,
            filesDropped=run.files_dropped,
        )
        del self.runs[run.msg_id]


# --------------------------------------------------------------------------
# Main loop


def _stdin_reader(lines: "queue.Queue[object]") -> None:
    stream = io.TextIOWrapper(sys.stdin.buffer, encoding="utf-8", errors="replace", newline="\n")
    for line in stream:
        lines.put(line)
    lines.put(None)  # EOF: the supervisor went away


def _heartbeat(host: KernelHost, stop: threading.Event) -> None:
    seq = 0
    while not stop.wait(HEARTBEAT_INTERVAL):
        seq += 1
        host.out.event("heartbeat", seq=seq, executionState=host.execution_state)


_CHILD_JOB = None


def contain_children() -> bool:
    """Windows: put this helper in a kill-on-close job object.

    Every kernel process tree started afterwards belongs to the job, so the
    kernels end with the helper even when the helper is killed (kernels other
    than ipykernel do not watch their parent). Uses only ctypes.
    """
    global _CHILD_JOB
    if sys.platform != "win32":
        return False
    try:
        import ctypes
        from ctypes import wintypes

        class IoCounters(ctypes.Structure):
            _fields_ = [
                (name, ctypes.c_ulonglong)
                for name in ("read_ops", "write_ops", "other_ops", "read_bytes", "write_bytes", "other_bytes")
            ]

        class BasicLimits(ctypes.Structure):
            _fields_ = [
                ("PerProcessUserTimeLimit", ctypes.c_int64),
                ("PerJobUserTimeLimit", ctypes.c_int64),
                ("LimitFlags", wintypes.DWORD),
                ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t),
                ("ActiveProcessLimit", wintypes.DWORD),
                ("Affinity", ctypes.c_size_t),
                ("PriorityClass", wintypes.DWORD),
                ("SchedulingClass", wintypes.DWORD),
            ]

        class ExtendedLimits(ctypes.Structure):
            _fields_ = [
                ("BasicLimitInformation", BasicLimits),
                ("IoInfo", IoCounters),
                ("ProcessMemoryLimit", ctypes.c_size_t),
                ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t),
                ("PeakJobMemoryUsed", ctypes.c_size_t),
            ]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateJobObjectW.restype = wintypes.HANDLE
        kernel32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
        kernel32.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
        kernel32.GetCurrentProcess.restype = wintypes.HANDLE
        kernel32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
        job = kernel32.CreateJobObjectW(None, None)
        if not job:
            return False
        limits = ExtendedLimits()
        limits.BasicLimitInformation.LimitFlags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not kernel32.SetInformationJobObject(job, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            return False
        if not kernel32.AssignProcessToJobObject(job, kernel32.GetCurrentProcess()):
            return False
        _CHILD_JOB = job  # the handle stays open (non-inheritable) for the helper's lifetime
        return True
    except Exception:
        return False


def kill_process_tree(pid: int | None) -> None:
    """Terminate a process and its descendants (best effort)."""
    if not pid:
        return
    if sys.platform == "win32":
        import subprocess

        taskkill = os.path.join(os.environ.get("SYSTEMROOT", r"C:\Windows"), "System32", "taskkill.exe")
        try:
            subprocess.run(
                [taskkill, "/PID", str(int(pid)), "/T", "/F"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=15,
                check=False,
                creationflags=0x08000000,  # CREATE_NO_WINDOW
            )
        except (OSError, subprocess.SubprocessError):
            pass
    else:
        import signal

        try:
            os.killpg(os.getpgid(int(pid)), signal.SIGKILL)
        except (OSError, ProcessLookupError):
            pass


def serve() -> int:
    protocol_stream = reserve_protocol_stdout()
    emitter = Emitter(protocol_stream)
    contained = contain_children()
    host = KernelHost(emitter, os.path.abspath(__file__))
    host.contained = contained
    lines: "queue.Queue[object]" = queue.Queue()
    threading.Thread(target=_stdin_reader, args=(lines,), name="stdin", daemon=True).start()
    stop = threading.Event()
    threading.Thread(target=_heartbeat, args=(host, stop), name="heartbeat", daemon=True).start()
    last_health_check = 0.0
    try:
        while True:
            # Drain pending requests without blocking kernel message handling.
            while True:
                try:
                    line = lines.get_nowait() if host.kc is not None else lines.get(timeout=0.25)
                except queue.Empty:
                    break
                if line is None:
                    host.shutting_down = True
                    host._finish_runs_on_loss("supervisorGone")
                    host._stop_kernel(now=True)
                    return 0
                text = str(line).strip()
                if not text:
                    continue
                try:
                    request = json.loads(text)
                    if not isinstance(request, dict):
                        raise ValueError("request must be a JSON object")
                except ValueError as error:
                    emitter.event("log", level="error", message=f"malformed request: {error}")
                    continue
                if not host.handle(request):
                    return 0
                if host.kc is None:
                    break
            host.pump(0.05 if host.kc is not None else 0.0)
            now = time.monotonic()
            if now - last_health_check >= 0.5:
                last_health_check = now
                host.check_kernel()
    finally:
        stop.set()
        emitter.closed = True


def launch_ipykernel(argv: list[str]) -> None:
    """Kernel entry point: ipykernel with interrupts that wake blocking calls.

    On Windows jupyter_client interrupts kernels through an event that
    ipykernel's parent poller turns into ``_thread.interrupt_main()``. That
    only takes effect between bytecodes, so ``time.sleep(60)`` would finish
    first. Raising SIGINT instead runs the C signal handler, which also wakes
    interruptible waits.
    """
    # The script directory (the helper's install location) must not shadow
    # project modules; IPython adds the working directory back.
    if sys.path and os.path.abspath(sys.path[0] or ".") == os.path.dirname(os.path.abspath(__file__)):
        del sys.path[0]
    if sys.platform == "win32":
        try:
            import signal

            import ipykernel.parentpoller as parentpoller

            if hasattr(parentpoller, "interrupt_main"):
                parentpoller.interrupt_main = lambda *args: signal.raise_signal(signal.SIGINT)
        except Exception:
            pass
    from ipykernel import kernelapp

    sys.argv = [sys.argv[0], *argv]
    kernelapp.launch_new_instance()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--ipykernel":
        launch_ipykernel(sys.argv[2:])
    else:
        exit_code = serve()
        try:
            sys.stderr.flush()
        finally:
            # Skip interpreter finalization: daemon threads may still be blocked
            # reading stdin, and every kernel has already been shut down.
            os._exit(exit_code)

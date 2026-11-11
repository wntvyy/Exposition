"""Tests for the kernel host helper.

Run with the interpreter whose kernels you want to exercise:

    python -m unittest discover -s workers/kernel-host/tests -v

Protocol tests start real ipykernel kernels; they are skipped when
jupyter_client or ipykernel is not importable by this interpreter.
"""

from __future__ import annotations

import csv
import io
import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from importlib import util

HERE = os.path.dirname(os.path.abspath(__file__))
HELPER = os.path.join(os.path.dirname(HERE), "exposition_kernel_host.py")
sys.path.insert(0, os.path.dirname(HERE))

import exposition_kernel_host as host  # noqa: E402

HAS_KERNEL = bool(util.find_spec("jupyter_client") and util.find_spec("ipykernel"))
HAS_SCIENCE = bool(util.find_spec("matplotlib") and util.find_spec("pandas"))


class TableGridTests(unittest.TestCase):
    def test_pandas_style_table(self) -> None:
        markup = """<div><style scoped>.x{}</style><table border="1" class="dataframe">
          <thead><tr style="text-align: right;"><th></th><th>x</th><th>y</th></tr></thead>
          <tbody><tr><th>0</th><td>1</td><td>a &amp; b</td></tr>
          <tr><th>1</th><td>2.5</td><td><script>alert(1)</script>c</td></tr></tbody></table></div>"""
        rows, header_rows = host.html_table_grid(markup)
        self.assertEqual(header_rows, 1)
        self.assertEqual(rows, [["", "x", "y"], ["0", "1", "a & b"], ["1", "2.5", "c"]])

    def test_spans_and_multirow_headers(self) -> None:
        markup = """<table><thead>
          <tr><th></th><th colspan="2">grp</th></tr>
          <tr><th>idx</th><th>a</th><th>b</th></tr></thead>
          <tbody><tr><th rowspan="2">k</th><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></tbody></table>"""
        rows, header_rows = host.html_table_grid(markup)
        self.assertEqual(header_rows, 1)
        self.assertEqual(rows[0], ["idx", "grp a", "grp b"])
        self.assertEqual(rows[1:], [["k", "1", "2"], ["k", "3", "4"]])

    def test_optional_end_tags_and_no_table(self) -> None:
        rows, _ = host.html_table_grid("<table><tr><th>a<th>b<tr><td>1<td>2</table>")
        self.assertEqual(rows, [["a", "b"], ["1", "2"]])
        self.assertIsNone(host.html_table_grid("<p>no table</p>"))

    def test_csv_round_trip(self) -> None:
        rows = [["a,b", 'q"t'], ["1", "line\nbreak"]]
        self.assertEqual(list(csv.reader(io.StringIO(host.grid_to_csv(rows)))), rows)

    def test_strip_ansi(self) -> None:
        self.assertEqual(host.strip_ansi("\x1b[0;31mValueError\x1b[0m: x"), "ValueError: x")


class Helper:
    """A real helper process speaking the JSON-lines protocol."""

    def __init__(self, cwd: str) -> None:
        env = {k: v for k, v in os.environ.items() if not k.upper().startswith("PYTHON")}
        env["PYTHONUTF8"] = "1"
        self.proc = subprocess.Popen(
            [sys.executable, "-u", HELPER],
            cwd=cwd,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.messages: "queue.Queue[dict]" = queue.Queue()
        self.events: list[dict] = []
        self.next_id = 0
        self.stderr: list[bytes] = []
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._drain_stderr, daemon=True).start()

    def _read(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            self.messages.put(json.loads(line.decode("utf-8")))
        self.messages.put({"type": "eof"})

    def _drain_stderr(self) -> None:
        assert self.proc.stderr is not None
        for line in self.proc.stderr:
            self.stderr.append(line)

    def request(self, cmd: str, timeout: float = 90, **fields: object) -> dict:
        self.next_id += 1
        req_id = self.next_id
        assert self.proc.stdin is not None
        self.proc.stdin.write((json.dumps({"id": req_id, "cmd": cmd, **fields}) + "\n").encode("utf-8"))
        self.proc.stdin.flush()
        deadline = time.time() + timeout
        while True:
            message = self.messages.get(timeout=max(0.1, deadline - time.time()))
            if message.get("type") == "eof":
                raise AssertionError("helper exited: " + b"".join(self.stderr).decode(errors="replace")[-2000:])
            if message.get("type") == "response" and message.get("id") == req_id:
                return message
            self.events.append(message)

    def wait_event(self, predicate, timeout: float = 90) -> dict:
        for event in self.events:
            if predicate(event):
                self.events.remove(event)
                return event
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                message = self.messages.get(timeout=max(0.1, deadline - time.time()))
            except queue.Empty:
                break
            if message.get("type") == "eof":
                raise AssertionError("helper exited: " + b"".join(self.stderr).decode(errors="replace")[-3000:])
            if predicate(message):
                return message
            self.events.append(message)
        raise AssertionError("timed out waiting for event")

    def run(self, run_id: str, code: str, output_dir: str, timeout: float = 90) -> tuple[list[dict], dict]:
        response = self.request("execute", run_id=run_id, code=code, output_dir=output_dir)
        assert response["ok"], response
        finished = self.wait_event(lambda e: e.get("event") == "run_finished" and e.get("run_id") == run_id, timeout)
        outputs = [e for e in self.events if e.get("run_id") == run_id]
        self.events = [e for e in self.events if e.get("run_id") != run_id]
        return outputs, finished

    def close(self) -> None:
        try:
            if self.proc.poll() is None:
                self.request("shutdown", timeout=30)
        except Exception:
            pass
        try:
            self.proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=30)
        for stream in (self.proc.stdin, self.proc.stdout, self.proc.stderr):
            try:
                if stream is not None:
                    stream.close()
            except OSError:
                pass


@unittest.skipUnless(HAS_KERNEL, "jupyter_client and ipykernel are required")
class ProtocolTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.workdir = tempfile.mkdtemp(prefix="kernel-host-test-")
        cls.helper = Helper(cls.workdir)
        hello = cls.helper.request("handshake", protocol=host.PROTOCOL_VERSION)
        assert hello["ok"], hello
        cls.hello = hello["result"]
        started = cls.helper.request("start", kernel={"kind": "python"}, cwd=cls.workdir)
        assert started["ok"], started
        cls.started = started["result"]

    @classmethod
    def tearDownClass(cls) -> None:
        cls.helper.close()

    def out_dir(self, run_id: str) -> str:
        return os.path.join(self.workdir, ".exposition", "artifacts", run_id)

    def test_handshake_reports_versions(self) -> None:
        self.assertEqual(self.hello["protocol"], 1)
        self.assertEqual(self.hello["helperVersion"], host.HELPER_VERSION)
        self.assertEqual(self.hello["python"]["executable"], sys.executable)
        self.assertTrue(self.hello["packages"]["jupyter_client"])
        self.assertEqual(self.started["kernel"]["language"], "python")
        self.assertIsInstance(self.started["pid"], int)

    def test_stream_and_result(self) -> None:
        outputs, finished = self.helper.run("r-stream", "print('hello')\n6*7", self.out_dir("r-stream"))
        self.assertEqual(finished["status"], "ok")
        text = "".join(e["text"] for e in outputs if e["event"] == "stream")
        self.assertEqual(text, "hello\n")
        result = [e for e in outputs if e["event"] == "display"][0]
        self.assertEqual(result["outputType"], "execute_result")
        self.assertEqual(result["text"], "42")
        self.assertTrue(any(e["event"] == "run_started" for e in outputs))

    def test_error_reports_traceback(self) -> None:
        outputs, finished = self.helper.run("r-error", "raise ValueError('bad value')", self.out_dir("r-error"))
        self.assertEqual(finished["status"], "error")
        error = [e for e in outputs if e["event"] == "error"][0]
        self.assertEqual(error["ename"], "ValueError")
        self.assertEqual(error["evalue"], "bad value")
        self.assertFalse(any("\x1b" in line for line in error["traceback"]))

    def test_large_stream_is_chunked(self) -> None:
        outputs, _ = self.helper.run("r-big", "print('x' * 100000)", self.out_dir("r-big"))
        chunks = [e["text"] for e in outputs if e["event"] == "stream"]
        self.assertTrue(all(len(c) <= host.STREAM_CHUNK for c in chunks))
        self.assertEqual(len("".join(chunks)), 100001)

    @unittest.skipUnless(HAS_SCIENCE, "matplotlib and pandas are required")
    def test_figure_and_table_files(self) -> None:
        code = (
            "import matplotlib\nimport matplotlib.pyplot as plt\nimport pandas as pd\n"
            "plt.plot([1, 2, 3], [1, 4, 9])\nplt.show()\n"
            "pd.DataFrame({'n': [1, 2], 'sq': [1.5, 4.0]})"
        )
        outputs, finished = self.helper.run("r-rich", code, self.out_dir("r-rich"), timeout=120)
        self.assertEqual(finished["status"], "ok")
        files = [f for e in outputs if e["event"] == "display" for f in e["files"]]
        png = [f for f in files if f["mediaType"] == "image/png"][0]
        with open(png["path"], "rb") as handle:
            data = handle.read()
        self.assertTrue(data.startswith(b"\x89PNG"))
        self.assertEqual(png["size"], len(data))
        table = [f for f in files if f["role"] == "table"][0]
        with open(table["path"], encoding="utf-8") as handle:
            rows = list(csv.reader(handle))
        self.assertEqual(rows, [["", "n", "sq"], ["0", "1", "1.5"], ["1", "2", "4.0"]])
        html_file = [f for f in files if f["mediaType"] == "text/html"][0]
        self.assertTrue(os.path.dirname(html_file["path"]) == self.out_dir("r-rich"))

    def test_interrupt_blocking_sleep(self) -> None:
        response = self.helper.request("execute", run_id="r-sleep", code="import time\ntime.sleep(60)",
                                       output_dir=self.out_dir("r-sleep"))
        self.assertTrue(response["ok"])
        self.helper.wait_event(lambda e: e.get("event") == "run_started" and e.get("run_id") == "r-sleep")
        time.sleep(0.5)
        started = time.time()
        self.assertTrue(self.helper.request("interrupt")["ok"])
        finished = self.helper.wait_event(
            lambda e: e.get("event") == "run_finished" and e.get("run_id") == "r-sleep", timeout=30
        )
        self.assertLess(time.time() - started, 10, "interrupt must wake time.sleep promptly")
        self.assertEqual(finished["status"], "error")
        error = self.helper.wait_event(lambda e: e.get("event") == "error" and e.get("run_id") == "r-sleep", 1)
        self.assertEqual(error["ename"], "KeyboardInterrupt")

    def test_zz_restart_and_kernel_death(self) -> None:
        self.helper.run("r-state", "marker = 123", self.out_dir("r-state"))
        restarted = self.helper.request("restart")
        self.assertTrue(restarted["ok"], restarted)
        outputs, finished = self.helper.run("r-after", "'marker' in globals()", self.out_dir("r-after"))
        self.assertEqual([e["text"] for e in outputs if e["event"] == "display"], ["False"])
        # Kill the kernel process from the outside: the helper must report it.
        pid = restarted["result"]["pid"]
        if sys.platform == "win32":
            subprocess.run(["taskkill", "/PID", str(pid), "/F"], capture_output=True, check=False)
        else:
            os.kill(pid, 9)
        died = self.helper.wait_event(lambda e: e.get("event") == "kernel_died", timeout=30)
        self.assertIn("exitCode", died)
        refused = self.helper.request("execute", run_id="r-dead", code="1", output_dir=self.out_dir("r-dead"))
        self.assertFalse(refused["ok"])
        self.assertEqual(refused["error"]["code"], "kernel.dead")
        recovered = self.helper.request("restart")
        self.assertTrue(recovered["ok"], recovered)
        _, finished = self.helper.run("r-recovered", "1 + 1", self.out_dir("r-recovered"))
        self.assertEqual(finished["status"], "ok")


@unittest.skipUnless(HAS_KERNEL, "jupyter_client and ipykernel are required")
class HandshakeTests(unittest.TestCase):
    def test_version_mismatch_and_ordering(self) -> None:
        workdir = tempfile.mkdtemp(prefix="kernel-host-hs-")
        helper = Helper(workdir)
        try:
            early = helper.request("status")
            self.assertEqual(early["error"]["code"], "protocol.handshakeRequired")
            mismatch = helper.request("handshake", protocol=999)
            self.assertEqual(mismatch["error"]["code"], "protocol.versionMismatch")
            self.assertTrue(helper.request("handshake", protocol=host.PROTOCOL_VERSION)["ok"])
            unknown = helper.request("frobnicate")
            self.assertEqual(unknown["error"]["code"], "protocol.unknownCommand")
            specs = helper.request("kernelspecs")
            self.assertTrue(specs["ok"])
            self.assertIsInstance(specs["result"]["kernelspecs"], list)
        finally:
            helper.close()
        self.assertEqual(helper.proc.returncode, 0)


if __name__ == "__main__":
    unittest.main()

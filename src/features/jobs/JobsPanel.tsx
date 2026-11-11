// Jobs panel: running and finished jobs, cancel, and a bounded output viewer.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { ListChecks, ShieldCheck, Square } from "lucide-react";

import type { JobSummary } from "../../bindings/JobSummary";
import type { OutputStream } from "../../bindings/OutputStream";
import {
  Banner,
  Button,
  Checkbox,
  EmptyState,
  IconButton,
  Select,
  StatusPill,
  Toolbar,
} from "../../ui/components";
import { EMPTY_OUTPUT, formatDuration, isActive, jobDurationMs, jobStatus } from "./model";
import { cancelJob, refreshJobs, selectJob, startJobsTracking, useJobs } from "./store";
import { requestWorkspaceTrust } from "./trust";
import "./jobs.css";

function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [active]);
  return now;
}

export function JobsPanel() {
  const jobs = useJobs((s) => s.jobs);
  const selected = useJobs((s) => s.selected);
  const showProbes = useJobs((s) => s.showProbes);
  const error = useJobs((s) => s.error);

  useEffect(() => startJobsTracking(), []);

  const visible = useMemo(
    () => jobs.filter((j) => showProbes || j.kind !== "probe").slice().reverse(),
    [jobs, showProbes],
  );
  const now = useNow(visible.some(isActive));
  const selectedJob = visible.find((j) => j.info.id === selected) ?? null;

  return (
    <div className="exp-jobs" data-testid="jobs-panel" aria-label="Jobs" role="region">
      <div className="exp-jobs__list">
        <Toolbar aria-label="Job list options" className="exp-jobs__toolbar">
          <Checkbox isSelected={showProbes} onChange={(value) => useJobs.setState({ showProbes: value })}>
            Show tool probes
          </Checkbox>
        </Toolbar>
        {error && (
          <Banner tone="danger" title="Could not load jobs">
            {error}
          </Banner>
        )}
        {visible.length === 0 ? (
          <EmptyState
            icon={ListChecks}
            title="No jobs yet"
            description="Builds, runs, and checks appear here while they run, and stay listed when they finish."
          />
        ) : (
          <ul className="exp-jobs__rows" aria-label="Job list">
            {visible.map((job) => (
              <JobRow key={job.info.id} job={job} now={now} selected={job.info.id === selected} />
            ))}
          </ul>
        )}
      </div>
      <div className="exp-jobs__detail">
        {selectedJob ? (
          <JobDetail job={selectedJob} now={now} />
        ) : (
          visible.length > 0 && <EmptyState title="Select a job" description="Its output and details appear here." />
        )}
      </div>
    </div>
  );
}

function JobStatusPill({ job, now, size }: { job: JobSummary; now?: number; size?: "sm" | "md" }) {
  const status = jobStatus(job);
  const detail = now === undefined ? undefined : formatDuration(jobDurationMs(job, now));
  return (
    <span data-testid="job-status" data-job-status={status.key}>
      <StatusPill status={status.status} label={status.label} detail={detail} size={size} />
    </span>
  );
}

function JobRow({ job, now, selected }: { job: JobSummary; now: number; selected: boolean }) {
  return (
    <li className="exp-jobs__row" data-job-id={job.info.id} data-selected={selected || undefined}>
      <button
        type="button"
        className="exp-jobs__select"
        aria-current={selected || undefined}
        onClick={() => selectJob(job.info.id)}
      >
        <span className="exp-jobs__label">{job.info.label}</span>
        <JobStatusPill job={job} now={now} size="sm" />
      </button>
      {isActive(job) && (
        <IconButton
          icon={Square}
          size="sm"
          aria-label={`Cancel ${job.info.label}`}
          isDisabled={job.state === "canceling"}
          onPress={() => void cancelJob(job.info.id)}
        />
      )}
    </li>
  );
}

type StreamFilter = "all" | OutputStream;

const STREAM_OPTIONS: { id: StreamFilter; label: string }[] = [
  { id: "all", label: "stdout and stderr" },
  { id: "stdout", label: "stdout only" },
  { id: "stderr", label: "stderr only" },
];

function JobDetail({ job, now }: { job: JobSummary; now: number }) {
  const output = useJobs((s) => s.outputs[job.info.id]) ?? EMPTY_OUTPUT;
  const [autoScroll, setAutoScroll] = useState(true);
  const [filter, setFilter] = useState<StreamFilter>("all");
  const viewer = useRef<HTMLPreElement>(null);
  const status = jobStatus(job);
  const trustRoot = job.refusal?.code === "untrustedWorkspace" ? job.refusal.workspaceRoot : null;

  useLayoutEffect(() => {
    if (autoScroll && viewer.current) viewer.current.scrollTop = viewer.current.scrollHeight;
  }, [output, autoScroll, filter]);

  const chunks = filter === "all" ? output.chunks : output.chunks.filter((c) => c.stream === filter);

  return (
    <div className="exp-jobs__job" data-testid="job-detail" data-job-id={job.info.id}>
      <div className="exp-jobs__heading">
        <h3 className="exp-jobs__title">{job.info.label}</h3>
        <JobStatusPill job={job} now={now} />
        {isActive(job) && (
          <Button
            variant="secondary"
            size="sm"
            icon={Square}
            isDisabled={job.state === "canceling"}
            onPress={() => void cancelJob(job.info.id)}
          >
            Cancel job
          </Button>
        )}
      </div>
      {trustRoot ? (
        <Banner
          tone="warning"
          title="Workspace not trusted — nothing was run"
          actions={
            <Button
              size="sm"
              icon={ShieldCheck}
              onPress={() => void requestWorkspaceTrust(trustRoot).then(() => refreshJobs())}
            >
              Trust workspace…
            </Button>
          }
        >
          {status.detail}
        </Banner>
      ) : (
        status.detail && (
          <Banner tone={status.status === "unavailable" ? "warning" : "danger"} title={status.label}>
            <span data-testid="job-status-detail">{status.detail}</span>
          </Banner>
        )
      )}
      {status.key === "canceled" && (
        <Banner tone="canceled" title="Canceled">
          The process tree was stopped; output below is partial.
        </Banner>
      )}
      <dl className="exp-jobs__facts">
        <dt>Program</dt>
        <dd>
          <code>{job.info.program}</code>
        </dd>
        <dt>Arguments</dt>
        <dd>
          <code>{job.info.args.length ? job.info.args.join(" ") : "(none)"}</code>
        </dd>
        <dt>Directory</dt>
        <dd>
          <code>{job.workingDirectory}</code>
        </dd>
        <dt>Policy</dt>
        <dd>{job.info.policy.kind === "trustedNative" ? "Trusted native" : `Restricted (${job.info.policy.backend})`}</dd>
        {job.pid !== null && (
          <>
            <dt>Process</dt>
            <dd data-testid="job-pid">{job.pid}</dd>
          </>
        )}
      </dl>
      <Toolbar aria-label="Output options" className="exp-jobs__toolbar">
        <Select<StreamFilter>
          aria-label="Output streams"
          options={STREAM_OPTIONS}
          value={filter}
          onChange={setFilter}
        />
        <Checkbox isSelected={autoScroll} onChange={setAutoScroll}>
          Auto-scroll
        </Checkbox>
        {output.truncated && (
          <span className="exp-muted" data-testid="output-truncated">
            Older output was dropped (log limit).
          </span>
        )}
      </Toolbar>
      <pre
        ref={viewer}
        className="exp-jobs__output"
        data-testid="job-output"
        tabIndex={0}
        aria-label={`Output of ${job.info.label}`}
      >
        {chunks.map((chunk) => (
          <span key={chunk.seq} className="exp-jobs__chunk" data-stream={chunk.stream}>
            {chunk.text}
          </span>
        ))}
      </pre>
      <p className="exp-jobs__legend exp-muted">
        Standard output in the default color; <span data-stream="stderr">standard error</span> marked with a bar.
      </p>
    </div>
  );
}

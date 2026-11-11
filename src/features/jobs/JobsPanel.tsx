// Jobs panel: running and finished jobs, cancel, and a bounded output viewer.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import type { JobSummary } from "../../bindings/JobSummary";
import type { OutputStream } from "../../bindings/OutputStream";
import { EMPTY_OUTPUT, formatDuration, isActive, jobDurationMs, jobStatus } from "./model";
import { cancelJob, selectJob, startJobsTracking, useJobs } from "./store";
import "./jobs.css";

function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
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
  const anyActive = visible.some(isActive);
  const now = useNow(anyActive);
  const selectedJob = visible.find((j) => j.info.id === selected) ?? null;

  return (
    <section className="exp-jobs" data-testid="jobs-panel" aria-label="Jobs">
      <div className="exp-jobs-list">
        <div className="exp-toolbar">
          <h2 className="exp-panel-title">Jobs</h2>
          <label className="exp-check">
            <input
              type="checkbox"
              checked={showProbes}
              onChange={(e) => useJobs.setState({ showProbes: e.currentTarget.checked })}
            />
            Show tool probes
          </label>
        </div>
        {error && (
          <p role="alert" className="exp-error">
            {error}
          </p>
        )}
        {visible.length === 0 ? (
          <p className="exp-empty">No jobs yet. Builds, runs, and checks appear here while they run.</p>
        ) : (
          <ul className="exp-job-rows" aria-label="Job list">
            {visible.map((job) => (
              <JobRow key={job.info.id} job={job} now={now} selected={job.info.id === selected} />
            ))}
          </ul>
        )}
      </div>
      <div className="exp-jobs-detail">
        {selectedJob ? (
          <JobDetail job={selectedJob} now={now} />
        ) : (
          <p className="exp-empty">Select a job to see its output.</p>
        )}
      </div>
    </section>
  );
}

function StatusPill({ job }: { job: JobSummary }) {
  const status = jobStatus(job);
  return (
    <span className={`exp-pill exp-tone-${status.tone}`} data-testid="job-status" data-status={status.key}>
      {status.label}
    </span>
  );
}

function JobRow({ job, now, selected }: { job: JobSummary; now: number; selected: boolean }) {
  const active = isActive(job);
  return (
    <li className="exp-job-row" data-job-id={job.info.id} data-selected={selected || undefined}>
      <button
        type="button"
        className="exp-job-select"
        aria-current={selected || undefined}
        onClick={() => selectJob(job.info.id)}
      >
        <span className="exp-job-label">{job.info.label}</span>
        <span className="exp-job-meta">
          <StatusPill job={job} />
          <span className="exp-job-duration" data-testid="job-duration">
            {formatDuration(jobDurationMs(job, now))}
          </span>
        </span>
      </button>
      {active && (
        <button
          type="button"
          className="exp-button exp-button-danger"
          disabled={job.state === "canceling"}
          aria-label={`Cancel ${job.info.label}`}
          onClick={() => void cancelJob(job.info.id)}
        >
          Cancel
        </button>
      )}
    </li>
  );
}

type StreamFilter = "all" | OutputStream;

function JobDetail({ job, now }: { job: JobSummary; now: number }) {
  const output = useJobs((s) => s.outputs[job.info.id]) ?? EMPTY_OUTPUT;
  const [autoScroll, setAutoScroll] = useState(true);
  const [filter, setFilter] = useState<StreamFilter>("all");
  const viewer = useRef<HTMLPreElement>(null);
  const status = jobStatus(job);

  useLayoutEffect(() => {
    if (autoScroll && viewer.current) viewer.current.scrollTop = viewer.current.scrollHeight;
  }, [output, autoScroll, filter]);

  const chunks = filter === "all" ? output.chunks : output.chunks.filter((c) => c.stream === filter);

  return (
    <div className="exp-job-detail" data-testid="job-detail" data-job-id={job.info.id}>
      <div className="exp-toolbar">
        <h3 className="exp-panel-title">{job.info.label}</h3>
        <StatusPill job={job} />
        <span className="exp-muted">{formatDuration(jobDurationMs(job, now))}</span>
        {isActive(job) && (
          <button
            type="button"
            className="exp-button exp-button-danger"
            disabled={job.state === "canceling"}
            onClick={() => void cancelJob(job.info.id)}
          >
            Cancel job
          </button>
        )}
      </div>
      <dl className="exp-job-facts">
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
      {status.detail && (
        <p className={status.tone === "danger" ? "exp-error" : "exp-muted"} data-testid="job-status-detail">
          {status.detail}
        </p>
      )}
      <div className="exp-toolbar" role="group" aria-label="Output options">
        <label className="exp-check">
          Show
          <select value={filter} onChange={(e) => setFilter(e.currentTarget.value as StreamFilter)}>
            <option value="all">stdout and stderr</option>
            <option value="stdout">stdout only</option>
            <option value="stderr">stderr only</option>
          </select>
        </label>
        <label className="exp-check">
          <input type="checkbox" checked={autoScroll} onChange={(e) => setAutoScroll(e.currentTarget.checked)} />
          Auto-scroll
        </label>
        {output.truncated && (
          <span className="exp-muted" data-testid="output-truncated">
            Older output was dropped (log limit).
          </span>
        )}
      </div>
      <pre
        ref={viewer}
        className="exp-output"
        data-testid="job-output"
        tabIndex={0}
        aria-label={`Output of ${job.info.label}`}
        aria-live="off"
      >
        {chunks.map((chunk) => (
          <span key={chunk.seq} className={`exp-out-${chunk.stream}`} data-stream={chunk.stream}>
            {chunk.text}
          </span>
        ))}
      </pre>
      <p className="exp-legend exp-muted">
        <span className="exp-out-stdout">stdout</span> · <span className="exp-out-stderr">stderr</span> (marked with a
        bar)
      </p>
    </div>
  );
}

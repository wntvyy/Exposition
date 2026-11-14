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
  InspectorHeader,
  KeyValueList,
  ListRow,
  Select,
  StatusPill,
  StatusText,
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

/** The job's state: quiet inline text in the list, the headline pill in its details. */
function JobState({ job, now, headline }: { job: JobSummary; now: number; headline?: boolean }) {
  const status = jobStatus(job);
  const detail = formatDuration(jobDurationMs(job, now));
  return (
    <span data-testid="job-status" data-job-status={status.key}>
      {headline ? (
        <StatusPill status={status.status} label={status.label} detail={detail} />
      ) : (
        <StatusText status={status.status} label={status.label} detail={detail} />
      )}
    </span>
  );
}

function JobRow({ job, now, selected }: { job: JobSummary; now: number; selected: boolean }) {
  return (
    <li className="exp-jobs__row" data-job-id={job.info.id}>
      <ListRow
        selected={selected}
        current={selected}
        title={
          // The button covers the whole row (jobs.css); Cancel stays above it.
          <button
            type="button"
            className="exp-jobs__select"
            aria-current={selected || undefined}
            onClick={() => selectJob(job.info.id)}
          >
            {job.info.label}
          </button>
        }
        meta={<JobState job={job} now={now} />}
        trailing={
          isActive(job) && (
            <IconButton
              icon={Square}
              size="sm"
              aria-label={`Cancel ${job.info.label}`}
              isDisabled={job.state === "canceling"}
              onPress={() => void cancelJob(job.info.id)}
            />
          )
        }
      />
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
      <InspectorHeader
        title={job.info.label}
        meta={<JobState job={job} now={now} headline />}
        actions={
          isActive(job) && (
            <Button
              variant="secondary"
              size="sm"
              icon={Square}
              isDisabled={job.state === "canceling"}
              onPress={() => void cancelJob(job.info.id)}
            >
              Cancel job
            </Button>
          )
        }
      />
      <div className="exp-jobs__body">
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
        <KeyValueList
          items={[
            { key: "program", label: "Program", value: job.info.program, code: true },
            {
              key: "args",
              label: "Arguments",
              value: job.info.args.length ? job.info.args.join(" ") : "(none)",
              code: true,
            },
            { key: "dir", label: "Directory", value: job.workingDirectory, code: true },
            {
              key: "policy",
              label: "Policy",
              value: job.info.policy.kind === "trustedNative" ? "Trusted native" : `Restricted (${job.info.policy.backend})`,
            },
            ...(job.pid !== null
              ? [{ key: "pid", label: "Process", value: job.pid, valueProps: { "data-testid": "job-pid" } }]
              : []),
          ]}
        />
        <Toolbar aria-label="Output options" className="exp-jobs__output-options">
          <Select<StreamFilter> aria-label="Output streams" options={STREAM_OPTIONS} value={filter} onChange={setFilter} />
          <Checkbox isSelected={autoScroll} onChange={setAutoScroll}>
            Auto-scroll
          </Checkbox>
          {output.truncated && (
            <span className="ui-meta" data-testid="output-truncated">
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
        <p className="exp-jobs__legend ui-meta">
          Standard output in the default color; <span data-stream="stderr">standard error</span> marked with a bar.
        </p>
      </div>
    </div>
  );
}

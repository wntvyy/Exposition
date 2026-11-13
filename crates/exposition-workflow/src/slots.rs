//! Application-wide execution slots shared by every active run.
//!
//! A step holds one slot while it executes: one of the global in-flight
//! steps plus one of its kind (restricted container, native process, agent
//! task, Lean audit — a Lean audit is a native process and holds both).
//! Acquisition never blocks: a step that finds a limit reached waits in its
//! run's scheduler and tries again; it is never refused for it.

use std::sync::{Arc, Mutex};

use exposition_graph::workflow::registry::Containment;

use crate::adapters::runner_ids;
use crate::model::{WorkflowConcurrencyLimits, WorkflowPlanStep};

/// The limits a step falls under (besides the global in-flight limit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotKind {
    Restricted,
    Native,
    Agent,
    /// A Lean audit: counts against the Lean and the native limit.
    Lean,
    /// An application service operation (only the global limit).
    Service,
}

impl SlotKind {
    pub(crate) fn of(step: &WorkflowPlanStep) -> Self {
        if step.runner == runner_ids::LEAN_AUDIT {
            return SlotKind::Lean;
        }
        match step.containment {
            Containment::RestrictedContainer => SlotKind::Restricted,
            Containment::NativeAccount => SlotKind::Native,
            Containment::ReadOnlyModel => SlotKind::Agent,
            Containment::ApplicationService => SlotKind::Service,
        }
    }
}

/// Steps holding a slot now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotCounts {
    pub in_flight: u32,
    pub restricted: u32,
    pub native: u32,
    pub agent: u32,
    pub lean: u32,
}

pub(crate) struct Slots {
    limits: WorkflowConcurrencyLimits,
    counts: Mutex<SlotCounts>,
}

/// One held slot, released when dropped.
pub(crate) struct SlotGuard {
    slots: Arc<Slots>,
    kind: SlotKind,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut c = self.slots.counts.lock().unwrap_or_else(|p| p.into_inner());
        c.in_flight = c.in_flight.saturating_sub(1);
        match self.kind {
            SlotKind::Restricted => c.restricted = c.restricted.saturating_sub(1),
            SlotKind::Native => c.native = c.native.saturating_sub(1),
            SlotKind::Agent => c.agent = c.agent.saturating_sub(1),
            SlotKind::Lean => {
                c.lean = c.lean.saturating_sub(1);
                c.native = c.native.saturating_sub(1);
            }
            SlotKind::Service => {}
        }
    }
}

impl Slots {
    pub(crate) fn new(limits: WorkflowConcurrencyLimits) -> Arc<Self> {
        Arc::new(Self { limits, counts: Mutex::new(SlotCounts::default()) })
    }

    pub(crate) fn limits(&self) -> &WorkflowConcurrencyLimits {
        &self.limits
    }

    pub(crate) fn counts(&self) -> SlotCounts {
        *self.counts.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A slot for a step of `kind`, or `None` while a limit it falls under is
    /// reached.
    pub(crate) fn try_acquire(self: &Arc<Self>, kind: SlotKind) -> Option<SlotGuard> {
        let l = &self.limits;
        let mut c = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        if c.in_flight >= l.max_in_flight_steps.max(1) {
            return None;
        }
        let fits = match kind {
            SlotKind::Restricted => c.restricted < l.restricted.max(1),
            SlotKind::Native => c.native < l.native.max(1),
            SlotKind::Agent => c.agent < l.agent.max(1),
            SlotKind::Lean => c.lean < l.lean.max(1) && c.native < l.native.max(1),
            SlotKind::Service => true,
        };
        if !fits {
            return None;
        }
        c.in_flight += 1;
        match kind {
            SlotKind::Restricted => c.restricted += 1,
            SlotKind::Native => c.native += 1,
            SlotKind::Agent => c.agent += 1,
            SlotKind::Lean => {
                c.lean += 1;
                c.native += 1;
            }
            SlotKind::Service => {}
        }
        Some(SlotGuard { slots: Arc::clone(self), kind })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_respect_every_limit_and_are_released_on_drop() {
        let slots = Slots::new(WorkflowConcurrencyLimits::default());
        let r1 = slots.try_acquire(SlotKind::Restricted).unwrap();
        let _r2 = slots.try_acquire(SlotKind::Restricted).unwrap();
        assert!(slots.try_acquire(SlotKind::Restricted).is_none(), "restricted 2");
        let lean = slots.try_acquire(SlotKind::Lean).unwrap();
        assert!(slots.try_acquire(SlotKind::Lean).is_none(), "lean 1");
        let _native = slots.try_acquire(SlotKind::Native).unwrap();
        assert_eq!(slots.counts().native, 2, "the Lean audit counts as native");
        assert!(slots.try_acquire(SlotKind::Native).is_none(), "native 2");
        assert!(slots.try_acquire(SlotKind::Service).is_none(), "4 in flight");
        assert!(slots.try_acquire(SlotKind::Agent).is_none(), "4 in flight");
        drop(r1);
        let agent = slots.try_acquire(SlotKind::Agent).unwrap();
        assert!(slots.try_acquire(SlotKind::Service).is_none());
        drop(lean);
        drop(agent);
        assert_eq!(slots.counts(), SlotCounts { in_flight: 2, restricted: 1, native: 1, agent: 0, lean: 0 });
    }
}

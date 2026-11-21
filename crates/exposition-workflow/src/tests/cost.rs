//! Cost regressions, asserted with work counters (never wall-clock time):
//! what a run-history listing decodes, and what the scheduler does while it
//! has nothing to do.

use super::*;
use crate::store::RunStore;

/// A store holding `n` copies (fresh ids) of a finished run.
fn store_with(f: &Fixture, n: usize) -> (RunStore, String, Vec<WorkflowRun>) {
    let id = f.save(numerical(false));
    let template = f.run_to_end(&id);
    let key = crate::plan::project_key(&f.root);
    let store = RunStore::open(Some(&f.db)).unwrap();
    let mut runs = vec![template.clone()];
    for i in 1..n {
        let mut run = template.clone();
        run.id = exposition_graph::WorkflowRunId::mint();
        run.started_at_ms += i as u64;
        store.put(&key, &run).unwrap();
        runs.push(run);
    }
    (store, key, runs)
}

#[test]
fn a_listing_decodes_only_the_records_that_changed_since_the_last_one() {
    let f = fixture();
    let (store, key, mut runs) = store_with(&f, 12);
    let decoded = |store: &RunStore| store.counters().bodies_decoded;
    let first = store.list(&key, None, 100).unwrap();
    assert_eq!(first.len(), 12);
    assert_eq!(decoded(&store), 12, "the first listing decodes every listed record");
    // Listing again reads no record text at all.
    let again = store.list(&key, None, 100).unwrap();
    assert_eq!(again, first);
    assert_eq!(decoded(&store), 12);
    // One run changes (as a run in progress does on every state change):
    // exactly that record is decoded, and the listing shows the change.
    let changed = runs.last_mut().unwrap();
    changed.acknowledged = !changed.acknowledged;
    changed.title = "Renamed".into();
    store.put(&key, changed).unwrap();
    let after = store.list(&key, None, 100).unwrap();
    assert_eq!(decoded(&store), 13);
    assert_eq!(after.iter().find(|s| s.id == changed.id).unwrap().title, "Renamed");
    // Every summary equals the one a full decode of the stored record gives.
    for summary in &after {
        let (_, run) = store.get(&summary.id).unwrap().unwrap();
        assert_eq!(*summary, crate::store::summary(&run));
    }
    // A workflow filter and a smaller limit list from the same summaries.
    let before = decoded(&store);
    assert_eq!(store.list(&key, Some(&runs[0].workflow), 5).unwrap().len(), 5);
    assert_eq!(store.list(&key, None, 100).unwrap().len(), 12);
    assert_eq!(decoded(&store), before);
}

#[test]
fn a_record_changed_by_anything_else_is_decoded_and_checked_again() {
    let f = fixture();
    let (store, key, runs) = store_with(&f, 4);
    assert_eq!(store.list(&key, None, 100).unwrap().len(), 4);
    // Another connection makes one record unreadable and another one
    // inconsistent with its index: both leave the listing at once.
    let other = rusqlite::Connection::open(&f.db).unwrap();
    other.execute("UPDATE runs SET body = '{\"id\": 1}' WHERE id = ?1", [runs[1].id.as_str()]).unwrap();
    other.execute("UPDATE runs SET state = 'running' WHERE id = ?1", [runs[2].id.as_str()]).unwrap();
    drop(other);
    let listed: Vec<_> = store.list(&key, None, 100).unwrap().into_iter().map(|s| s.id).collect();
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert!(!listed.contains(&runs[1].id) && !listed.contains(&runs[2].id));
    // Deleting (recoverably), restoring, removing, and compacting keep the listing truthful.
    assert!(store.set_deleted(&runs[0].id, Some(5)).unwrap());
    assert_eq!(store.list(&key, None, 100).unwrap().len(), 1);
    assert!(store.set_deleted(&runs[0].id, None).unwrap());
    assert_eq!(store.list(&key, None, 100).unwrap().len(), 2);
    assert!(store.remove(&runs[3].id).unwrap());
    store.compact().unwrap();
    let listed: Vec<_> = store.list(&key, None, 100).unwrap().into_iter().map(|s| s.id).collect();
    assert_eq!(listed, vec![runs[0].id.clone()]);
}

#[test]
fn the_service_listing_shows_every_state_change_of_a_run() {
    // The history read after each run event is current, although unchanged
    // records are not decoded again.
    let f = fixture();
    let id = f.save(numerical(false));
    let first = f.run_to_end(&id);
    let listed = f.service.runs(None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, WorkflowRunState::Succeeded);
    let second = f.run_to_end(&id);
    let before = f.service.store_counters().bodies_decoded;
    let listed = f.service.runs(None).unwrap();
    let mut ids: Vec<_> = listed.iter().map(|s| s.id.clone()).collect();
    ids.sort();
    let mut expected = vec![first.id.clone(), second.id.clone()];
    expected.sort();
    assert_eq!(ids, expected);
    assert!(listed.iter().all(|s| s.state == WorkflowRunState::Succeeded && s.steps_done == s.steps_total));
    assert_eq!(f.service.store_counters().bodies_decoded - before, 1, "only the new record is decoded");
}

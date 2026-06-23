//! Engine-wide GC-pressure mailbox mechanics (#27g): the engine posts a collection request to every
//! live store's mailbox when committed GC bytes cross the threshold; a store reads-and-clears *its
//! own*, leaving the others'; dead mailboxes are pruned.

#![allow(clippy::unwrap_used)]

use std::sync::atomic::Ordering::Relaxed;

use super::*;
use crate::config::Config;

fn engine(threshold: usize) -> Engine {
    let mut cfg = Config::new();
    cfg.gc_memory_threshold(threshold);
    Engine::new(&cfg).unwrap()
}

#[test]
fn below_threshold_does_not_post() {
    let engine = engine(1000);
    let m = engine.register_gc_request();
    engine.add_gc_committed(500);
    assert!(!m.load(Relaxed), "no request below the threshold");
    assert_eq!(engine.gc_committed_bytes(), 500);
}

#[test]
fn crossing_threshold_posts_to_every_mailbox() {
    let engine = engine(1000);
    let a = engine.register_gc_request();
    let b = engine.register_gc_request();
    engine.add_gc_committed(1200); // > 1000
    assert!(a.load(Relaxed) && b.load(Relaxed), "all stores notified");
}

#[test]
fn one_store_clears_only_its_own_mailbox() {
    let engine = engine(1000);
    let a = engine.register_gc_request();
    let b = engine.register_gc_request();
    engine.add_gc_committed(2000);
    // `a` services the request (read-and-clear); `b` is untouched, so it still collects later.
    assert!(a.swap(false, Relaxed));
    assert!(!a.load(Relaxed), "serviced");
    assert!(b.load(Relaxed), "other store still has its request");
}

#[test]
fn dead_mailboxes_are_pruned_without_panic() {
    let engine = engine(1000);
    let a = engine.register_gc_request();
    {
        let _b = engine.register_gc_request();
    } // `_b`'s store dropped → its `Weak` is dead
      // Posting prunes the dead mailbox and still notifies the live one.
    engine.add_gc_committed(2000);
    assert!(a.load(Relaxed));
    // A second post (after pruning) still works.
    a.store(false, Relaxed);
    engine.add_gc_committed(1);
    assert!(a.load(Relaxed));
}

#[test]
fn committed_total_tracks_add_and_sub() {
    let engine = engine(usize::MAX); // never posts
    engine.add_gc_committed(4096);
    engine.add_gc_committed(2048);
    assert_eq!(engine.gc_committed_bytes(), 6144);
    engine.sub_gc_committed(4096);
    assert_eq!(engine.gc_committed_bytes(), 2048);
}

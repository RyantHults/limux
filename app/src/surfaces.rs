//! Surface registry — tracks all active Ghostty surfaces.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk4::prelude::*;

use crate::ghostty_sys::ghostty_surface_t;
use crate::split::SurfaceId;

/// Per-surface state.
pub struct SurfaceEntry {
    pub surface: ghostty_surface_t,
    pub gl_area: gtk4::GLArea,
    pub im_context: gtk4::IMMulticontext,
}

thread_local! {
    static REGISTRY: RefCell<SurfaceRegistry> = RefCell::new(SurfaceRegistry::new());
}

struct SurfaceRegistry {
    surfaces: HashMap<SurfaceId, SurfaceEntry>,
    /// Pending surfaces: pre-allocated ID → GLArea (not yet realized)
    pending: HashMap<SurfaceId, gtk4::GLArea>,
    next_id: SurfaceId,
}

impl SurfaceRegistry {
    fn new() -> Self {
        Self {
            surfaces: HashMap::new(),
            pending: HashMap::new(),
            next_id: 0,
        }
    }
}

/// Register a new surface. Returns its ID.
pub fn register(entry: SurfaceEntry) -> SurfaceId {
    REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        let id = reg.next_id;
        reg.next_id += 1;
        reg.surfaces.insert(id, entry);
        id
    })
}

/// Register a surface with a specific pre-allocated ID.
pub fn register_with_id(id: SurfaceId, entry: SurfaceEntry) {
    REGISTRY.with(|r| {
        r.borrow_mut().surfaces.insert(id, entry);
    });
}

/// Remove a surface by ID.
pub fn unregister(id: SurfaceId) -> Option<SurfaceEntry> {
    REGISTRY.with(|r| r.borrow_mut().surfaces.remove(&id))
}

/// Get the ghostty_surface_t handle for a surface.
pub fn get_handle(id: SurfaceId) -> Option<ghostty_surface_t> {
    REGISTRY.with(|r| {
        r.borrow()
            .surfaces
            .get(&id)
            .map(|e| e.surface)
    })
}

/// Get the GLArea widget for a surface.
pub fn get_gl_area(id: SurfaceId) -> Option<gtk4::GLArea> {
    REGISTRY.with(|r| {
        r.borrow()
            .surfaces
            .get(&id)
            .map(|e| e.gl_area.clone())
    })
}

/// Queue render on all surfaces (called from tick).
pub fn queue_render_all() {
    REGISTRY.with(|r| {
        for entry in r.borrow().surfaces.values() {
            entry.gl_area.queue_render();
        }
    });
}

/// Get all surface IDs.
pub fn all_ids() -> Vec<SurfaceId> {
    REGISTRY.with(|r| r.borrow().surfaces.keys().copied().collect())
}

/// Get the first available ghostty_surface_t (for clipboard fallback).
pub fn any_handle() -> Option<ghostty_surface_t> {
    REGISTRY.with(|r| {
        r.borrow()
            .surfaces
            .values()
            .next()
            .map(|e| e.surface)
    })
}

/// Pre-allocate a surface ID (for split pane creation before realize).
pub fn pre_allocate_id() -> SurfaceId {
    REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        let id = reg.next_id;
        reg.next_id += 1;
        id
    })
}

/// Register a pending (not yet realized) surface with its GLArea.
pub fn register_pending(id: SurfaceId, gl_area: gtk4::GLArea) {
    REGISTRY.with(|r| {
        r.borrow_mut().pending.insert(id, gl_area);
    });
}

/// Get the GLArea for a pending surface.
pub fn get_pending_gl_area(id: SurfaceId) -> Option<gtk4::GLArea> {
    REGISTRY.with(|r| {
        r.borrow().pending.get(&id).cloned()
    })
}

/// Remove a pending entry.
pub fn remove_pending(id: SurfaceId) {
    REGISTRY.with(|r| {
        r.borrow_mut().pending.remove(&id);
    });
}

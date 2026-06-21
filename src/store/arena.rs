//! A generic index-keyed arena backing the store's entity tables. A handle is a `u32` index into
//! the backing `Vec`; entries are grow-only (freed only when the owning `StoreInner` drops).

/// An index-keyed arena; a handle is a `u32` index into the backing `Vec`.
#[derive(Debug)]
pub(crate) struct Arena<E>(Vec<E>);

impl<E> Default for Arena<E> {
    fn default() -> Self {
        Arena(Vec::new())
    }
}

impl<E> Arena<E> {
    pub(super) fn alloc(&mut self, entity: E) -> u32 {
        let index = self.0.len() as u32;
        self.0.push(entity);
        index
    }

    pub(super) fn get(&self, index: u32) -> &E {
        &self.0[index as usize]
    }

    pub(super) fn get_mut(&mut self, index: u32) -> &mut E {
        &mut self.0[index as usize]
    }

    pub(super) fn len(&self) -> u32 {
        self.0.len() as u32
    }
}

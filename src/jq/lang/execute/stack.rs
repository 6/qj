//! Port of jq 1.8.1's `exec_stack.h`, plus the block types execute.c keeps on it
//! (`struct frame`, `struct forkpoint`, data values).
//!
//! jq's stack is "a directed forest of variably sized blocks": one memory region holding
//! three interleaved linked lists (the data stack, the call frames and the fork points).
//! Blocks are allocated strictly LIFO, and a block is only freed when it is popped while
//! it is the most recently allocated one (`stack_pop_will_free`). Popping any other block
//! just moves the list head, leaving the block alive for the fork point that still
//! refers to it. That is what lets backtracking resume a generator with its old data
//! stack and frames.
//!
//! Here the region is a `Vec` of blocks: block pointer `p` (1-based) is `slots[p - 1]`,
//! `0` is jq's null pointer, and the limit (the last allocated block) is `slots.len()`.
//! Frame entries (closure parameters and local variables) live in two side arenas that
//! follow the same LIFO discipline as the frame blocks that own them: a frame block is
//! only freed once every block allocated after it is gone, so its entries are then at
//! the top of the arenas and are truncated with it.

use crate::jq::value::Value;

/// jq's `stack_ptr`: a block pointer, `0` meaning none.
pub(super) type StackPtr = u32;

/// Return address of the top-level frame (jq's `retaddr == 0`).
pub(super) const NO_RETADDR: u32 = u32::MAX;

/// `struct closure`: a function body plus the frame it closes over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Closure {
    /// The callee's function id (jq's `struct bytecode*`).
    pub func: u32,
    /// The closed frame (jq's `env`).
    pub env: StackPtr,
}

/// `struct frame`: a jq function call frame.
#[derive(Clone, Copy, Debug)]
pub(super) struct Frame {
    /// The callee's function id (`bc`).
    pub func: u32,
    /// The lexically enclosing frame (`env`), followed by `frame_get_level`.
    pub env: StackPtr,
    /// Data stack pointer to unwind to on `RET` (`retdata`).
    pub retdata: StackPtr,
    /// Global pc to return to, or [`NO_RETADDR`] for the top-level frame.
    pub retaddr: u32,
    /// Index of this frame's closure parameters in [`Stack::closures`].
    pub closures: u32,
    /// Index of this frame's local variables in [`Stack::locals`].
    pub locals: u32,
}

/// `struct forkpoint`: a saved machine state to resume from when backtracking.
pub(super) struct ForkPoint {
    pub saved_data_stack: StackPtr,
    pub saved_curr_frame: StackPtr,
    pub path_len: usize,
    pub subexp_nest: i32,
    pub value_at_path: Value,
    /// Global pc of the instruction that made the fork point.
    pub return_address: u32,
}

enum Block {
    Value(Value),
    Frame(Frame),
    Fork(ForkPoint),
}

struct Slot {
    /// `*stack_block_next(s, p)`.
    next: StackPtr,
    block: Block,
}

/// `struct stack` plus the frame-entry arenas.
#[derive(Default)]
pub(super) struct Stack {
    slots: Vec<Slot>,
    /// Closure parameters of every live frame (`union frame_entry` closures).
    pub closures: Vec<Closure>,
    /// Local variables of every live frame (`union frame_entry` locals).
    pub locals: Vec<Value>,
}

impl Stack {
    /// The stack pointer of the last allocated block (`s->limit`), `0` when empty.
    #[inline]
    pub fn limit(&self) -> StackPtr {
        self.slots.len() as StackPtr
    }

    /// `stack_pop_will_free`.
    #[inline]
    pub fn pop_will_free(&self, p: StackPtr) -> bool {
        p == self.limit()
    }

    /// `*stack_block_next(s, p)`.
    #[inline]
    pub fn next(&self, p: StackPtr) -> StackPtr {
        self.slots[p as usize - 1].next
    }

    /// Whether nothing is allocated (`stack_reset`'s precondition).
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty() && self.closures.is_empty() && self.locals.is_empty()
    }

    /// `stack_reset`: frees the region (keeping its capacity for the next run).
    pub fn reset(&mut self) {
        debug_assert!(self.slots.is_empty(), "stack freed while not empty");
        self.slots.clear();
        self.closures.clear();
        self.locals.clear();
    }

    #[inline]
    fn push_block(&mut self, next: StackPtr, block: Block) -> StackPtr {
        self.slots.push(Slot { next, block });
        self.limit()
    }

    // ---- data values ----------------------------------------------------------

    /// `stack_push`'s block operation: pushes `v` on the list headed by `top`, returning
    /// the new head.
    #[inline]
    pub fn push_value(&mut self, top: StackPtr, v: Value) -> StackPtr {
        self.push_block(top, Block::Value(v))
    }

    /// `stack_pop`: moves the value out when the block is freed, else copies it.
    /// Returns the value and the new list head.
    #[inline]
    pub fn pop_value(&mut self, top: StackPtr) -> (Value, StackPtr) {
        if self.pop_will_free(top) {
            let slot = self.slots.pop().expect("non-empty stack");
            match slot.block {
                Block::Value(v) => (v, slot.next),
                _ => unreachable!("stack_pop on a non-value block"),
            }
        } else {
            let slot = &self.slots[top as usize - 1];
            match &slot.block {
                Block::Value(v) => (v.clone(), slot.next),
                _ => unreachable!("stack_pop on a non-value block"),
            }
        }
    }

    /// `stack_popn`: like [`Stack::pop_value`], but a block that stays alive gets `null`
    /// instead of keeping a copy (the saved fork point sees `null`).
    #[inline]
    pub fn popn_value(&mut self, top: StackPtr) -> (Value, StackPtr) {
        if self.pop_will_free(top) {
            self.pop_value(top)
        } else {
            let slot = &mut self.slots[top as usize - 1];
            match &mut slot.block {
                Block::Value(v) => (std::mem::take(v), slot.next),
                _ => unreachable!("stack_popn on a non-value block"),
            }
        }
    }

    /// The value in block `p` (`*(jv*)stack_block(s, p)`).
    #[inline]
    pub fn value(&self, p: StackPtr) -> &Value {
        match &self.slots[p as usize - 1].block {
            Block::Value(v) => v,
            _ => unreachable!("not a value block"),
        }
    }

    // ---- frames ---------------------------------------------------------------

    /// Allocates a frame block whose `next` is `caller`, with room for its entries
    /// (the caller fills in the closures and locals).
    #[inline]
    pub fn push_frame(&mut self, caller: StackPtr, frame: Frame) -> StackPtr {
        self.push_block(caller, Block::Frame(frame))
    }

    #[inline]
    pub fn frame(&self, p: StackPtr) -> &Frame {
        match &self.slots[p as usize - 1].block {
            Block::Frame(f) => f,
            _ => unreachable!("not a frame block"),
        }
    }

    #[inline]
    pub fn frame_mut(&mut self, p: StackPtr) -> &mut Frame {
        match &mut self.slots[p as usize - 1].block {
            Block::Frame(f) => f,
            _ => unreachable!("not a frame block"),
        }
    }

    /// `frame_pop`'s block operation: frees the frame (and its locals) when it is the
    /// last allocated block. Returns the caller frame (`next`).
    #[inline]
    pub fn pop_frame(&mut self, p: StackPtr) -> StackPtr {
        if self.pop_will_free(p) {
            let slot = self.slots.pop().expect("non-empty stack");
            match slot.block {
                Block::Frame(f) => {
                    debug_assert!(f.closures as usize <= self.closures.len());
                    debug_assert!(f.locals as usize <= self.locals.len());
                    self.closures.truncate(f.closures as usize);
                    self.locals.truncate(f.locals as usize);
                }
                _ => unreachable!("frame_pop on a non-frame block"),
            }
            slot.next
        } else {
            self.next(p)
        }
    }

    // ---- fork points ------------------------------------------------------------

    /// Allocates a fork point whose `next` is `fork_top`.
    #[inline]
    pub fn push_fork(&mut self, fork_top: StackPtr, fork: ForkPoint) -> StackPtr {
        self.push_block(fork_top, Block::Fork(fork))
    }

    /// Pops the fork point `p`, which must be the last allocated block (as it always is
    /// in `stack_restore`). Returns it and the next fork point.
    #[inline]
    pub fn pop_fork(&mut self, p: StackPtr) -> (ForkPoint, StackPtr) {
        debug_assert!(self.pop_will_free(p));
        let slot = self.slots.pop().expect("non-empty stack");
        match slot.block {
            Block::Fork(f) => (f, slot.next),
            _ => unreachable!("not a fork block"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_blocks_are_copied_and_the_limit_is_freed() {
        let mut s = Stack::default();
        let a = s.push_value(0, Value::from(1));
        let b = s.push_value(a, Value::from(2));
        // A fork point above `b` keeps `b` alive when it's popped.
        let f = s.push_fork(
            0,
            ForkPoint {
                saved_data_stack: b,
                saved_curr_frame: 0,
                path_len: 0,
                subexp_nest: 0,
                value_at_path: Value::Null,
                return_address: 0,
            },
        );
        assert!(!s.pop_will_free(b));
        let (v, top) = s.pop_value(b);
        assert_eq!(v, Value::from(2));
        assert_eq!(top, a);
        assert_eq!(s.limit(), f);
        // popn leaves null behind for the fork point.
        let (v, top) = s.popn_value(a);
        assert_eq!(v, Value::from(1));
        assert_eq!(top, 0);
        assert!(s.value(a).is_null());
        let (fork, next) = s.pop_fork(f);
        assert_eq!(fork.saved_data_stack, b);
        assert_eq!(next, 0);
        // Now `b` is the limit and is freed by the pop.
        let (v, top) = s.pop_value(b);
        assert_eq!(v, Value::from(2));
        assert_eq!(top, a);
        assert_eq!(s.limit(), a);
        let (v, top) = s.pop_value(a);
        assert!(v.is_null());
        assert_eq!(top, 0);
        assert!(s.is_empty());
    }

    #[test]
    fn frames_free_their_entries_only_when_freed() {
        let mut s = Stack::default();
        let f1 = s.push_frame(
            0,
            Frame {
                func: 0,
                env: 0,
                retdata: 0,
                retaddr: NO_RETADDR,
                closures: 0,
                locals: 0,
            },
        );
        s.locals.push(Value::from("x"));
        let v = s.push_value(0, Value::Null);
        // f1 is not the limit: popping it leaves its locals.
        assert_eq!(s.pop_frame(f1), 0);
        assert_eq!(s.locals.len(), 1);
        let _ = s.pop_value(v);
        assert_eq!(s.pop_frame(f1), 0);
        assert!(s.is_empty());
    }
}

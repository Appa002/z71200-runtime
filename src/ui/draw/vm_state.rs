use super::TaggedWord;
use super::traits::{HasRegister, HasStack};
use std::{collections::HashMap, usize};

// ::: ---- Basic VM State Implementation --- ::
pub(super) struct VMState {
    regs: HashMap<usize, TaggedWord>,
    stack: Vec<TaggedWord>,
}
impl VMState {
    pub fn new() -> Self {
        VMState {
            regs: HashMap::new(),
            stack: Vec::new(),
        }
    }
}
impl HasRegister for VMState {
    fn regs_get(&mut self, k: usize) -> Option<TaggedWord> {
        self.regs.get(&k).cloned()
    }

    fn regs_set(&mut self, k: usize, v: TaggedWord) -> () {
        self.regs.insert(k, v);
    }
}
impl HasStack for VMState {
    fn stack_pop(&mut self) -> Option<TaggedWord> {
        self.stack.pop()
    }

    fn stack_push(&mut self, v: TaggedWord) -> () {
        self.stack.push(v);
    }
}

use rustc_hash::FxHashMap;

use crate::object::Object;

#[derive(Clone, Debug, Default)]
pub struct Bytecode {
    pub instructions: Vec<u8>,
    pub constants: Vec<Object>,
    pub line_number_table: Vec<(usize, usize)>,
    pub num_cache_slots: u16,
    pub max_stack_depth: u16,
    /// Maximum number of registers used by this function/program.
    pub register_count: u16,
    /// Maps **named** global variables to their slot indices.
    /// Note that this map only contains the *exported* names. Slots
    /// allocated by inner closures for captured locals (e.g. an
    /// IIFE's parameter mirror) live above the highest slot seen in
    /// this map; embedders that allocate fresh runtime globals must
    /// start from `next_global_slot`, not `globals_table.len()`.
    pub globals_table: FxHashMap<String, u16>,
    /// One past the highest global slot the compiler emitted, including
    /// private slots not present in `globals_table`. Embedders use this
    /// as the next-available index when defining runtime globals via
    /// `ScriptState::set_global`. Without this, a fresh runtime
    /// global could be assigned an index already in use by an inner
    /// closure for one of its captured names — silent state
    /// corruption that the user can't see from outside.
    pub next_global_slot: u16,
}

impl Bytecode {
    pub fn new(
        instructions: Vec<u8>,
        constants: Vec<Object>,
        line_number_table: Vec<(usize, usize)>,
    ) -> Self {
        Self {
            instructions,
            constants,
            line_number_table,
            num_cache_slots: 0,
            max_stack_depth: 0,
            register_count: 0,
            globals_table: FxHashMap::default(),
            next_global_slot: 0,
        }
    }

    pub fn with_cache_slots(
        instructions: Vec<u8>,
        constants: Vec<Object>,
        line_number_table: Vec<(usize, usize)>,
        num_cache_slots: u16,
        max_stack_depth: u16,
        register_count: u16,
    ) -> Self {
        Self {
            instructions,
            constants,
            line_number_table,
            num_cache_slots,
            max_stack_depth,
            register_count,
            globals_table: FxHashMap::default(),
            next_global_slot: 0,
        }
    }
}

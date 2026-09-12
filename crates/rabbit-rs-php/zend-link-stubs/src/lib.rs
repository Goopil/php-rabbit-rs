//! Dummy Zend symbol definitions for the `rabbit-rs-php` lib test harness.
//!
//! The publish buffer state machine drives `PublishBuffer`'s synchronous
//! flush paths, which pull ext-php-rs exception machinery into the harness
//! link. On x86_64 Linux the linker's GC keeps a small set of Zend symbols as
//! strong undefined references; the harness runs without a PHP engine, and
//! the BIND_NOW hardening on Debian/Ubuntu makes the dynamic loader abort at
//! startup on those unresolved symbols — which broke `cargo nextest --list`
//! in CI before any test could run. The harness never executes a Zend call
//! (the no-engine raise path panics first), so these definitions exist purely
//! to satisfy the loader: functions panic on entry, data is zeroed space.
//! Dev-dependency only — the cdylib links the real PHP library and never
//! sees this crate. If an ext-php-rs upgrade changes the referenced symbol
//! set, the loader error names the missing symbol — append it here. List
//! derived with `nm -u` on the x86_64-unknown-linux-gnu test harness.

macro_rules! stub {
    ($name:ident) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $name() {
            panic!("zend link stub called without a PHP engine");
        }
    };
}

stub!(instanceof_function_slow);
stub!(zend_lookup_class_ex);
stub!(zval_ptr_dtor);
stub!(zend_is_iterable);
stub!(zend_array_count);
stub!(zend_hash_get_current_data_ex);
stub!(zend_hash_get_current_key_type_ex);
stub!(zend_hash_get_current_key_zval_ex);
stub!(zend_hash_move_forward_ex);
stub!(zend_objects_store_del);
stub!(gc_possible_root);
stub!(__zend_malloc);
stub!(_emalloc);
stub!(_efree);

/// Never read without an engine; zeroed space satisfies the relocations.
#[unsafe(no_mangle)]
pub static executor_globals: [u64; 8192] = [0; 8192];

/// Never read without an engine; null class entry satisfies the relocations.
#[unsafe(no_mangle)]
pub static zend_ce_traversable: usize = 0;

use crate::wasm_conventions;
use anyhow::{anyhow, bail, Error};
use std::cmp;
use walrus::ir::Value;
use walrus::FunctionBuilder;
use walrus::{
    ir::MemArg, ConstExpr, ExportItem, FunctionId, GlobalId, GlobalKind, InstrSeqBuilder, MemoryId,
    Module, ValType,
};

pub const PAGE_SIZE: u32 = 1 << 16;
const DEFAULT_THREAD_STACK_SIZE: u32 = 1 << 21; // 2MB

/// MemArg for i32 atomic operations (thread counter, lock).  Natural
/// alignment for i32 atomics is 4.
const ATOMIC_MEM_ARG_I32: MemArg = MemArg {
    align: 4,
    offset: 0,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PtrType {
    I32,
    I64,
}

impl PtrType {
    fn val_type(self) -> ValType {
        match self {
            PtrType::I32 => ValType::I32,
            PtrType::I64 => ValType::I64,
        }
    }

    fn zero_const(self) -> ConstExpr {
        match self {
            PtrType::I32 => ConstExpr::Value(Value::I32(0)),
            PtrType::I64 => ConstExpr::Value(Value::I64(0)),
        }
    }

    fn is_64(self) -> bool {
        matches!(self, PtrType::I64)
    }
}

#[derive(Clone, Copy)]
pub struct ThreadCount(walrus::LocalId);

pub fn is_enabled(module: &Module) -> bool {
    match wasm_conventions::get_memory(module) {
        Ok(memory) => module.memories.get(memory).shared,
        Err(_) => false,
    }
}

pub fn run(module: &mut Module) -> Result<Option<ThreadCount>, Error> {
    if !is_enabled(module) {
        return Ok(None);
    }

    let memory = wasm_conventions::get_memory(module)?;
    let ptr = detect_ptr_type(module)?;

    let static_data_align = 4;
    let static_data_pages = 1;
    let (base, addr) =
        allocate_static_data(module, memory, static_data_pages, static_data_align, ptr)?;

    let mem = module.memories.get(memory);
    assert!(mem.shared);
    assert!(mem.import.is_some());
    assert!(mem.data_segments.is_empty());

    let tls = Tls {
        init: delete_synthetic_func(module, "__wasm_init_tls")?,
        size: delete_synthetic_global(module, "__tls_size")?,
        align: delete_synthetic_global(module, "__tls_align")?,
        base: wasm_conventions::get_tls_base(module)
            .ok_or_else(|| anyhow!("failed to find tls base"))?,
    };

    let thread_counter_addr: i64 = addr as i64;

    let stack_alloc = module
        .globals
        .add_local(ptr.val_type(), true, false, ptr.zero_const());

    let temp_stack =
        (base + static_data_pages as u64 * PAGE_SIZE as u64) & !(static_data_align as u64 - 1);

    const _: () = assert!(DEFAULT_THREAD_STACK_SIZE % PAGE_SIZE == 0);

    let stack_size_init = if ptr.is_64() {
        ConstExpr::Value(Value::I64(DEFAULT_THREAD_STACK_SIZE as i64))
    } else {
        ConstExpr::Value(Value::I32(DEFAULT_THREAD_STACK_SIZE as i32))
    };

    let stack = Stack {
        pointer: wasm_conventions::get_stack_pointer(module)
            .ok_or_else(|| anyhow!("failed to find stack pointer"))?,
        temp: temp_stack as i64,
        temp_lock: thread_counter_addr + 4,
        alloc: stack_alloc,
        size: module
            .globals
            .add_local(ptr.val_type(), true, false, stack_size_init),
        ptr,
    };

    let _ = module.exports.add("__stack_alloc", stack.alloc);

    let thread_count = inject_start(module, &tls, &stack, thread_counter_addr, memory, ptr)?;
    inject_destroy(module, &tls, &stack, memory, ptr)?;

    Ok(Some(thread_count))
}

fn detect_ptr_type(module: &Module) -> Result<PtrType, Error> {
    let heap_base = module
        .exports
        .iter()
        .filter(|e| e.name == "__heap_base")
        .find_map(|e| match e.item {
            ExportItem::Global(id) => Some(id),
            _ => None,
        });
    let heap_base = match heap_base {
        Some(idx) => idx,
        None => bail!("failed to find `__heap_base` for detecting pointer width"),
    };
    let global = module.globals.get(heap_base);
    match global.ty {
        ValType::I32 => Ok(PtrType::I32),
        ValType::I64 => Ok(PtrType::I64),
        _ => bail!("`__heap_base` has unexpected type {:?}", global.ty),
    }
}

impl ThreadCount {
    pub fn wrap_start(self, builder: &mut FunctionBuilder, start: FunctionId) {
        builder.func_body().local_get(self.0).if_else(
            None,
            |_| {},
            |body| {
                body.call(start);
            },
        );
    }
}

fn delete_synthetic_func(module: &mut Module, name: &str) -> Result<FunctionId, Error> {
    match delete_synthetic_export(module, name)? {
        walrus::ExportItem::Function(f) => Ok(f),
        _ => bail!("`{name}` must be a function"),
    }
}

fn delete_synthetic_global(module: &mut Module, name: &str) -> Result<u32, Error> {
    let id = match delete_synthetic_export(module, name)? {
        walrus::ExportItem::Global(g) => g,
        _ => bail!("`{name}` must be a global"),
    };
    let g = match &module.globals.get(id).kind {
        walrus::GlobalKind::Local(g) => g,
        walrus::GlobalKind::Import(_) => bail!("`{name}` must not be an imported global"),
    };
    match g {
        ConstExpr::Value(Value::I32(v)) => Ok(*v as u32),
        ConstExpr::Value(Value::I64(v)) => Ok(*v as u32),
        _ => bail!("`{name}` was not an integer constant"),
    }
}

fn delete_synthetic_export(module: &mut Module, name: &str) -> Result<ExportItem, Error> {
    let item = module
        .exports
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| anyhow!("failed to find `{name}`"))?;
    let ret = item.item;
    let id = item.id();
    module.exports.delete(id);
    Ok(ret)
}

fn allocate_static_data(
    module: &mut Module,
    memory: MemoryId,
    pages: u32,
    align: u32,
    ptr: PtrType,
) -> Result<(u64, u64), Error> {
    let heap_base = module
        .exports
        .iter()
        .filter(|e| e.name == "__heap_base")
        .find_map(|e| match e.item {
            ExportItem::Global(id) => Some(id),
            _ => None,
        });
    let heap_base = match heap_base {
        Some(idx) => idx,
        None => bail!("failed to find `__heap_base` for injecting thread id"),
    };

    let (base, address) = {
        let global = module.globals.get_mut(heap_base);
        let expected_ty = ptr.val_type();
        if global.ty != expected_ty {
            bail!(
                "the `__heap_base` global doesn't have the expected type {:?}",
                expected_ty
            );
        }
        if global.mutable {
            bail!("the `__heap_base` global is unexpectedly mutable");
        }
        let (base, address, new_kind) = match (&mut global.kind, ptr) {
            (GlobalKind::Local(ConstExpr::Value(Value::I32(n))), PtrType::I32) => {
                let address = (*n as u64 + (align as u64 - 1)) & !(align as u64 - 1);
                let base = *n as i64;
                let new_offset = *n + (pages * PAGE_SIZE) as i32;
                (base, address, ConstExpr::Value(Value::I32(new_offset)))
            }
            (GlobalKind::Local(ConstExpr::Value(Value::I64(n))), PtrType::I64) => {
                let address = (*n as u64 + (align as u64 - 1)) & !(align as u64 - 1);
                let base = *n;
                let new_offset = *n + (pages * PAGE_SIZE) as i64;
                (base, address, ConstExpr::Value(Value::I64(new_offset)))
            }
            _ => bail!("`__heap_base` not a locally defined integer of the right type"),
        };
        global.kind = GlobalKind::Local(new_kind);
        (base as u64, address)
    };

    let memory = module.memories.get_mut(memory);
    memory.initial += u64::from(pages);
    memory.maximum = memory.maximum.map(|m| cmp::max(m, memory.initial));

    Ok((base, address))
}

struct Tls {
    init: walrus::FunctionId,
    size: u32,
    align: u32,
    base: GlobalId,
}

struct Stack {
    pointer: GlobalId,
    temp: i64,
    temp_lock: i64,
    alloc: GlobalId,
    size: GlobalId,
    ptr: PtrType,
}

/// Push a pointer-sized constant onto the instruction stack.
macro_rules! ptr_const {
    ($body:expr, $ptr:expr, $val:expr) => {
        if $ptr.is_64() {
            $body.i64_const($val as i64);
        } else {
            $body.i32_const($val as i32);
        }
    };
}

/// Convert a pointer-sized value to f64 for calling malloc/free on wasm64.
/// On wasm64, __wbindgen_malloc/__wbindgen_free use f64 ABI for pointers.
macro_rules! ptr_to_f64 {
    ($body:expr, $ptr:expr) => {
        if $ptr.is_64() {
            $body.unop(walrus::ir::UnaryOp::F64ConvertSI64);
        }
    };
}

/// Convert an f64 return from malloc/free back to pointer-sized int.
macro_rules! f64_to_ptr {
    ($body:expr, $ptr:expr) => {
        if $ptr.is_64() {
            $body.unop(walrus::ir::UnaryOp::I64TruncSF64);
        }
    };
}

/// Convert a pointer-sized value to i32 (for if conditions, select).
macro_rules! ptr_to_i32 {
    ($body:expr, $ptr:expr) => {
        if $ptr.is_64() {
            $body.unop(walrus::ir::UnaryOp::I32WrapI64);
        }
    };
}

fn inject_start(
    module: &mut Module,
    tls: &Tls,
    stack: &Stack,
    thread_counter_addr: i64,
    memory: MemoryId,
    ptr: PtrType,
) -> Result<ThreadCount, Error> {
    use walrus::ir::*;

    let vt = ptr.val_type();
    let local = module.locals.add(vt);
    let thread_count = module.locals.add(vt);
    let stack_size = module.locals.add(vt);

    let malloc = find_function(module, "__wbindgen_malloc")?;

    let prev_start = wasm_conventions::get_start(module);
    let mut builder = FunctionBuilder::new(&mut module.types, &[vt], &[]);

    if let Ok(prev_start) | Err(Some(prev_start)) = prev_start {
        builder.func_body().call(prev_start);
    }

    let mut body = builder.func_body();

    let add_op = if ptr.is_64() {
        BinaryOp::I64Add
    } else {
        BinaryOp::I32Add
    };

    // Thread counter is an i32 value in memory.  The address is
    // pointer-sized (i64 on wasm64), but the value is i32.
    // atomic_rmw I32 returns i32; convert to pointer-sized for local_tee.
    ptr_const!(body, ptr, thread_counter_addr);
    body.i32_const(1);
    body.atomic_rmw(memory, AtomicOp::Add, AtomicWidth::I32, ATOMIC_MEM_ARG_I32);
    // Extend i32 → i64 on wasm64 for the local
    if ptr.is_64() {
        body.unop(walrus::ir::UnaryOp::I64ExtendSI32);
    }
    body.local_tee(thread_count);
    // if needs i32 condition
    ptr_to_i32!(body, ptr);
    body.if_else(
            None,
            |body| {
                // if stack_size != 0 (nonzero test needs i32 condition)
                body.local_get(stack_size);
                ptr_to_i32!(body, ptr);
                body.if_else(
                    None,
                    |body| {
                        body.local_get(stack_size).global_set(stack.size);
                    },
                    |_| (),
                );

                // local = malloc(stack.size, 16)
                // On wasm64, malloc takes (f64, f64) and returns f64.
                with_temp_stack(body, memory, stack, |body| {
                    body.global_get(stack.size);
                    ptr_to_f64!(body, ptr);
                    ptr_const!(body, ptr, 16);
                    ptr_to_f64!(body, ptr);
                    body.call(malloc);
                    f64_to_ptr!(body, ptr);
                    body.local_tee(local);
                });

                body.global_set(stack.alloc);

                // stack_pointer = base + stack.size
                body.global_get(stack.alloc)
                    .global_get(stack.size)
                    .binop(add_op)
                    .global_set(stack.pointer);
            },
            |_| {},
        );

    // TLS init: malloc(tls.size, tls.align)
    ptr_const!(body, ptr, tls.size as i64);
    ptr_to_f64!(body, ptr);
    ptr_const!(body, ptr, tls.align as i64);
    ptr_to_f64!(body, ptr);
    body.call(malloc);
    f64_to_ptr!(body, ptr);
    body.global_set(tls.base);
    body.global_get(tls.base);
    body.call(tls.init);

    let id = builder.finish(vec![stack_size], &mut module.funcs);
    module.start = Some(id);

    Ok(ThreadCount(thread_count))
}

fn inject_destroy(
    module: &mut Module,
    tls: &Tls,
    stack: &Stack,
    memory: MemoryId,
    ptr: PtrType,
) -> Result<(), Error> {
    use walrus::ir::*;

    let free = find_function(module, "__wbindgen_free")?;

    let vt = ptr.val_type();
    let mut builder = FunctionBuilder::new(&mut module.types, &[vt, vt, vt], &[]);
    builder.name("__wbindgen_thread_destroy".into());

    let mut body = builder.func_body();

    let tls_base = module.locals.add(vt);
    let stack_alloc = module.locals.add(vt);
    let stack_size = module.locals.add(vt);

    // if (tls_base) { free(tls_base, tls.size, tls.align) }
    // else { free(tls.base, tls.size, tls.align); tls.base = MIN }
    body.local_get(tls_base);
    ptr_to_i32!(body, ptr);
    body.if_else(
        None,
        |body| {
            body.local_get(tls_base);
            ptr_to_f64!(body, ptr);
            ptr_const!(body, ptr, tls.size as i64);
            ptr_to_f64!(body, ptr);
            ptr_const!(body, ptr, tls.align as i64);
            ptr_to_f64!(body, ptr);
            body.call(free);
        },
        |body| {
            body.global_get(tls.base);
            ptr_to_f64!(body, ptr);
            ptr_const!(body, ptr, tls.size as i64);
            ptr_to_f64!(body, ptr);
            ptr_const!(body, ptr, tls.align as i64);
            ptr_to_f64!(body, ptr);
            body.call(free);

            // set tls.base = MIN to trigger invalid memory
            if ptr.is_64() {
                body.i64_const(i64::MIN);
            } else {
                body.i32_const(i32::MIN);
            }
            body.global_set(tls.base);
        },
    );

    // if (stack_alloc) { free(stack_alloc, select(stack_size, DEFAULT), 16) }
    // else { with_temp_stack(free(stack.alloc, stack.size, 16)); stack.alloc = 0 }
    body.local_get(stack_alloc);
    ptr_to_i32!(body, ptr);
    body.if_else(
        None,
        |body| {
            // free(stack_alloc, select(stack_size, DEFAULT_STACK_SIZE), 16)
            // select: val1 = stack_size, val2 = DEFAULT, cond = stack_size (i32)
            body.local_get(stack_alloc);
            ptr_to_f64!(body, ptr);

            body.local_get(stack_size); // val1 for select (i64 on wasm64)
            if ptr.is_64() {
                body.i64_const(DEFAULT_THREAD_STACK_SIZE as i64); // val2
            } else {
                body.i32_const(DEFAULT_THREAD_STACK_SIZE as i32); // val2
            }
            body.local_get(stack_size); // cond — needs i32
            ptr_to_i32!(body, ptr);
            body.select(None);
            ptr_to_f64!(body, ptr);

            ptr_const!(body, ptr, 16);
            ptr_to_f64!(body, ptr);
            body.call(free);
        },
        |body| {
            with_temp_stack(body, memory, stack, |body| {
                body.global_get(stack.alloc);
                ptr_to_f64!(body, ptr);
                body.global_get(stack.size);
                ptr_to_f64!(body, ptr);
                ptr_const!(body, ptr, 16);
                ptr_to_f64!(body, ptr);
                body.call(free);
            });

            ptr_const!(body, ptr, 0);
            body.global_set(stack.alloc);
        },
    );

    let destroy_id = builder.finish(vec![tls_base, stack_alloc, stack_size], &mut module.funcs);
    module.exports.add("__wbindgen_thread_destroy", destroy_id);

    Ok(())
}

fn find_function(module: &Module, name: &str) -> Result<FunctionId, Error> {
    let e = module
        .exports
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| anyhow!("failed to find `{name}`"))?;
    match e.item {
        walrus::ExportItem::Function(f) => Ok(f),
        _ => bail!("`{name}` wasn't a function"),
    }
}

fn with_temp_stack(
    body: &mut InstrSeqBuilder<'_>,
    memory: MemoryId,
    stack: &Stack,
    block: impl Fn(&mut InstrSeqBuilder<'_>),
) {
    use walrus::ir::*;

    let ptr = stack.ptr;

    // All atomics operate on i32 values in memory (lock, counter).
    // Only the address is pointer-sized (i64 on wasm64).

    ptr_const!(body, ptr, stack.temp);
    body.global_set(stack.pointer);

    body.loop_(None, |loop_| {
        let loop_id = loop_.id();

        if ptr.is_64() {
            // cmpxchg i32: [i64 addr, i32 expected, i32 replacement] -> i32
            loop_
                .i64_const(stack.temp_lock) // addr
                .i32_const(0) // expected
                .i32_const(1) // replacement
                .cmpxchg(memory, AtomicWidth::I32, ATOMIC_MEM_ARG_I32)
                .if_else(
                    None,
                    |body| {
                        // memory.atomic.wait32: [i64 addr, i32 expected, i64 timeout] -> i32
                        body.i64_const(stack.temp_lock) // addr
                            .i32_const(1) // expected value
                            .i64_const(-1) // timeout (infinite)
                            .atomic_wait(memory, ATOMIC_MEM_ARG_I32, false)
                            .drop()
                            .br(loop_id);
                    },
                    |_| {},
                );
        } else {
            loop_
                .i32_const(stack.temp_lock as i32)
                .i32_const(0)
                .i32_const(1)
                .cmpxchg(memory, AtomicWidth::I32, ATOMIC_MEM_ARG_I32)
                .if_else(
                    None,
                    |body| {
                        body.i32_const(stack.temp_lock as i32)
                            .i32_const(1)
                            .i64_const(-1)
                            .atomic_wait(memory, ATOMIC_MEM_ARG_I32, false)
                            .drop()
                            .br(loop_id);
                    },
                    |_| {},
                );
        }
    });

    block(body);

    // Store 0 to unlock: [i64 addr, i32 value] -> store i32 atomic
    ptr_const!(body, ptr, stack.temp_lock);
    body.i32_const(0);
    body.store(memory, StoreKind::I32 { atomic: true }, ATOMIC_MEM_ARG_I32);

    // Notify: [i64 addr, i32 count] -> i32
    ptr_const!(body, ptr, stack.temp_lock);
    body.i32_const(1);
    body.atomic_notify(memory, ATOMIC_MEM_ARG_I32)
        .drop();
}

#[cfg(test)]
mod tests;

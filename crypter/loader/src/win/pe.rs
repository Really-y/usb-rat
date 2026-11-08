//! Minimal x64 reflective PE mapper.
//!
//! Handles: headers, sections, base relocations (DIR64), imports, section
//! protections, and TLS callbacks (best-effort). Then returns the entry
//! point so the caller can jump to it.
//!
//! Caveats: no CFG/SEH registration, no delay-load imports, no forwarded
//! export handling. Reflective-loading a full Rust/CRT binary is version-
//! and linker-sensitive — this MUST be validated on the target Windows build.

use std::ffi::c_void;

use windows::core::PCSTR;
use windows::Win32::System::Diagnostics::Debug::{
    RtlAddFunctionTable, IMAGE_RUNTIME_FUNCTION_ENTRY,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
use windows::Win32::System::Memory::{
    VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READ,
    PAGE_PROTECTION_FLAGS, PAGE_READONLY, PAGE_READWRITE, VIRTUAL_ALLOCATION_TYPE,
};

const PE_SIGNATURE: u32 = 0x0000_4550; // "PE\0\0"
const OPT_MAGIC_PE32_PLUS: u16 = 0x20B;
const DIR_IMPORT: usize = 1;
const DIR_EXCEPTION: usize = 3;
const DIR_BASERELOC: usize = 5;
const DIR_TLS: usize = 9;
const REL_BASED_ABSOLUTE: u16 = 0;
const REL_BASED_DIR64: u16 = 10;
const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const SCN_MEM_READ: u32 = 0x4000_0000;
const SCN_MEM_WRITE: u32 = 0x8000_0000;
const DLL_PROCESS_ATTACH: u32 = 1;

#[repr(C)]
struct FileHeader {
    machine: u16,
    number_of_sections: u16,
    time_date_stamp: u32,
    pointer_to_symbol_table: u32,
    number_of_symbols: u32,
    size_of_optional_header: u16,
    characteristics: u16,
}

#[repr(C)]
struct DataDirectory {
    virtual_address: u32,
    size: u32,
}

#[repr(C)]
struct OptionalHeader64 {
    magic: u16,
    major_linker_version: u8,
    minor_linker_version: u8,
    size_of_code: u32,
    size_of_initialized_data: u32,
    size_of_uninitialized_data: u32,
    address_of_entry_point: u32,
    base_of_code: u32,
    image_base: u64,
    section_alignment: u32,
    file_alignment: u32,
    major_operating_system_version: u16,
    minor_operating_system_version: u16,
    major_image_version: u16,
    minor_image_version: u16,
    major_subsystem_version: u16,
    minor_subsystem_version: u16,
    win32_version_value: u32,
    size_of_image: u32,
    size_of_headers: u32,
    checksum: u32,
    subsystem: u16,
    dll_characteristics: u16,
    size_of_stack_reserve: u64,
    size_of_stack_commit: u64,
    size_of_heap_reserve: u64,
    size_of_heap_commit: u64,
    loader_flags: u32,
    number_of_rva_and_sizes: u32,
    data_directory: [DataDirectory; 16],
}

#[repr(C)]
struct NtHeaders64 {
    signature: u32,
    file_header: FileHeader,
    optional_header: OptionalHeader64,
}

#[repr(C)]
struct SectionHeader {
    name: [u8; 8],
    virtual_size: u32,
    virtual_address: u32,
    size_of_raw_data: u32,
    pointer_to_raw_data: u32,
    pointer_to_relocations: u32,
    pointer_to_linenumbers: u32,
    number_of_relocations: u16,
    number_of_linenumbers: u16,
    characteristics: u32,
}

#[repr(C)]
struct ImportDescriptor {
    original_first_thunk: u32,
    time_date_stamp: u32,
    forwarder_chain: u32,
    name: u32,
    first_thunk: u32,
}

#[repr(C)]
struct BaseRelocation {
    virtual_address: u32,
    size_of_block: u32,
}

#[repr(C)]
struct TlsDirectory64 {
    start_address_of_raw_data: u64,
    end_address_of_raw_data: u64,
    address_of_index: u64,
    address_of_callbacks: u64,
    size_of_zero_fill: u32,
    characteristics: u32,
}

/// Map `pe` into this process and return its entry point address.
pub unsafe fn map_into_self(pe: &[u8]) -> Result<*mut c_void, String> {
    if pe.len() < 0x100 {
        return Err("image too small".into());
    }
    let src = pe.as_ptr();
    let e_lfanew = *(src.add(0x3C) as *const i32) as usize;
    if e_lfanew == 0 || e_lfanew + std::mem::size_of::<NtHeaders64>() > pe.len() {
        return Err("bad e_lfanew".into());
    }
    let nt = src.add(e_lfanew) as *const NtHeaders64;
    if (*nt).signature != PE_SIGNATURE {
        return Err("bad PE signature".into());
    }
    let opt = &(*nt).optional_header;
    if opt.magic != OPT_MAGIC_PE32_PLUS {
        return Err("not a PE32+ (x64) image".into());
    }

    let size_of_image = opt.size_of_image as usize;
    let size_of_headers = (opt.size_of_headers as usize).min(pe.len());
    let preferred = opt.image_base as usize;

    // Reserve + commit the whole image read-write.
    let mem = VirtualAlloc(
        None,
        size_of_image,
        VIRTUAL_ALLOCATION_TYPE(MEM_COMMIT.0 | MEM_RESERVE.0),
        PAGE_READWRITE,
    );
    if mem.is_null() {
        return Err("VirtualAlloc failed".into());
    }
    let base = mem as usize;

    std::ptr::copy_nonoverlapping(src, mem as *mut u8, size_of_headers);

    let fh = &(*nt).file_header;
    let sections_off = e_lfanew + 4 + std::mem::size_of::<FileHeader>()
        + fh.size_of_optional_header as usize;
    if sections_off + fh.number_of_sections as usize * std::mem::size_of::<SectionHeader>()
        > pe.len()
    {
        return Err("section table out of bounds".into());
    }
    let sections = src.add(sections_off) as *const SectionHeader;
    let nsec = fh.number_of_sections as usize;

    for i in 0..nsec {
        let s = &*sections.add(i);
        if s.size_of_raw_data == 0 {
            continue;
        }
        let src_off = s.pointer_to_raw_data as usize;
        let raw = s.size_of_raw_data as usize;
        if src_off + raw > pe.len() {
            return Err("section raw data out of bounds".into());
        }
        let dst = (base + s.virtual_address as usize) as *mut u8;
        std::ptr::copy_nonoverlapping(src.add(src_off), dst, raw);
    }

    // Base relocations (image preferred base may differ from where we mapped).
    let delta = base.wrapping_sub(preferred) as isize;
    if delta != 0 {
        let rva = opt.data_directory[DIR_BASERELOC].virtual_address as usize;
        let size = opt.data_directory[DIR_BASERELOC].size as usize;
        if rva != 0 && size != 0 {
            apply_relocations(base, rva, size, delta)?;
        }
    }

    resolve_imports(base, opt)?;

    // Section protections.
    for i in 0..nsec {
        let s = &*sections.add(i);
        let sz = s.virtual_size.max(s.size_of_raw_data) as usize;
        if sz == 0 {
            continue;
        }
        let mut protect = PAGE_READONLY;
        if s.characteristics & SCN_MEM_EXECUTE != 0 {
            protect = PAGE_EXECUTE_READ;
        } else if s.characteristics & SCN_MEM_WRITE != 0 {
            protect = PAGE_READWRITE;
        } else if s.characteristics & SCN_MEM_READ == 0 {
            protect = PAGE_READWRITE;
        }
        let mut old = PAGE_PROTECTION_FLAGS(0);
        let _ = VirtualProtect(
            (base + s.virtual_address as usize) as *const c_void,
            sz,
            protect,
            &mut old,
        );
    }

    // Register x64 unwind info (.pdata) so SEH / panics inside the mapped
    // image can unwind. Without this, exceptions in the payload fail-fast.
    let exc_rva = opt.data_directory[DIR_EXCEPTION].virtual_address as usize;
    let exc_size = opt.data_directory[DIR_EXCEPTION].size as usize;
    let entry_size = std::mem::size_of::<IMAGE_RUNTIME_FUNCTION_ENTRY>();
    if exc_rva != 0 && exc_size >= entry_size {
        let count = exc_size / entry_size;
        let table =
            std::slice::from_raw_parts((base + exc_rva) as *const IMAGE_RUNTIME_FUNCTION_ENTRY, count);
        let _ = RtlAddFunctionTable(table, base as u64);
    }

    // TLS callbacks (best effort — static TLS slot allocation is not done).
    let tls_rva = opt.data_directory[DIR_TLS].virtual_address as usize;
    if tls_rva != 0 {
        let tls = (base + tls_rva) as *const TlsDirectory64;
        let callbacks = (*tls).address_of_callbacks as *const u64;
        if !callbacks.is_null() {
            for i in 0..64 {
                let cb = *callbacks.add(i);
                if cb == 0 {
                    break;
                }
                let f: extern "system" fn(*mut c_void, u32, *mut c_void) =
                    std::mem::transmute(cb as usize);
                f(base as *mut c_void, DLL_PROCESS_ATTACH, std::ptr::null_mut());
            }
        }
    }

    Ok((base + opt.address_of_entry_point as usize) as *mut c_void)
}

unsafe fn apply_relocations(
    base: usize,
    rva: usize,
    size: usize,
    delta: isize,
) -> Result<(), String> {
    let mut off = 0usize;
    while off + std::mem::size_of::<BaseRelocation>() <= size {
        let block = (base + rva + off) as *const BaseRelocation;
        let block_va = (*block).virtual_address as usize;
        let block_size = (*block).size_of_block as usize;
        if block_size < 8 || off + block_size > size {
            break;
        }
        let count = (block_size - 8) / 2;
        let entries = (block as *const u8).add(8) as *const u16;
        for i in 0..count {
            let e = *entries.add(i);
            let typ = e >> 12;
            let ofs = (e & 0x0FFF) as usize;
            let addr = (base + block_va + ofs) as *mut u8;
            match typ {
                REL_BASED_ABSOLUTE => {}
                REL_BASED_DIR64 => {
                    let p = addr as *mut usize;
                    *p = (*p).wrapping_add(delta as usize);
                }
                _ => {} // HIGHLOW etc. don't occur in x64 images
            }
        }
        off += block_size;
    }
    Ok(())
}

unsafe fn resolve_imports(base: usize, opt: &OptionalHeader64) -> Result<(), String> {
    let import_rva = opt.data_directory[DIR_IMPORT].virtual_address as usize;
    if import_rva == 0 {
        return Ok(());
    }
    let mut desc = (base + import_rva) as *const ImportDescriptor;
    let mut guard = 0usize;
    loop {
        let d = &*desc;
        if d.name == 0 {
            break;
        }
        let dll = PCSTR::from_raw((base + d.name as usize) as *const u8);
        let hmod = LoadLibraryA(dll).map_err(|e| format!("LoadLibrary failed: {e}"))?;

        let first_thunk = (base + d.first_thunk as usize) as *mut u64;
        let orig = if d.original_first_thunk != 0 {
            (base + d.original_first_thunk as usize) as *const u64
        } else {
            first_thunk as *const u64
        };

        let mut idx = 0usize;
        loop {
            let entry = *orig.add(idx);
            if entry == 0 {
                break;
            }
            let addr: usize = if entry & 0x8000_0000_0000_0000 != 0 {
                // import by ordinal
                let ordinal = (entry & 0xFFFF) as usize;
                GetProcAddress(hmod, PCSTR::from_raw(ordinal as *const u8))
                    .map(|f| f as usize)
                    .unwrap_or(0)
            } else {
                let name_rva = (entry & 0x7FFF_FFFF) as usize;
                // skip the 2-byte hint preceding the name
                let name = PCSTR::from_raw((base + name_rva + 2) as *const u8);
                GetProcAddress(hmod, name)
                    .map(|f| f as usize)
                    .unwrap_or(0)
            };
            if addr == 0 {
                return Err(format!("unresolved import in module at rva {:#x}", d.name));
            }
            *first_thunk.add(idx) = addr as u64;
            idx += 1;
            if idx > 0x10000 {
                return Err("import thunk runaway".into());
            }
        }

        desc = desc.add(1);
        guard += 1;
        if guard > 0x1000 {
            return Err("import descriptor runaway".into());
        }
    }
    Ok(())
}

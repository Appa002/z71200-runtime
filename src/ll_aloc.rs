/* General Layout:

BOF ||free(bool) next(u32)...(padding)|| ||free(bool) next...(padding)|| ||free(bool) next...(padding)|| EOF
if next == 0x0 then no next node => all memory until EOF is free
aloc (n):
    find the first node with size > n, where size EOF_ptr - node_edge_ptr and is free
    place n bytes + padding into `...`
    alloc new node after `...` set the new node to be free and the old node to not be free
    rewire so that
        found_node -> new_node -> old_rhs_node (if exist)

free (ptr):
    the given ptr is what is returned from aloc and so just after the `next` u32 ie.
    ||00 00 00 00 00 FF FF FF ... ||
free> ** ~~~~~~~~~~~ ___________..
        ^          ^
        next      `ptr`

    this means find next like next := ptr - 4
    and is_free like := ptr - 4 - 1

    -> flip the free flag to yes
    -> check the next block if also free, if yes adjust our `next` ptr to skip that block (do this greedily until finding the first non-free block)
*/

//TODO: The three pointer kids we have for a block (ie block_ptr, data_ptr, free_ptr) could be captured in the type system better (ie. writing a block head to a free ptr upgrades that location to a block_ptr)

use anyhow::{Result, anyhow};

const WORD: usize = core::mem::size_of::<usize>(); // 4 of 8
const IS_FREE_BYTE_OFF: usize = 0;
const NEXT_PTR_BYTE_OFF: usize = IS_FREE_BYTE_OFF + 1 + (WORD - 1); // skip first byte and align to the next word
const DATA_PTR_BYTE_OFF: usize = NEXT_PTR_BYTE_OFF + WORD; // size is word

// compile time sanity
const HEADER_SIZE: usize = DATA_PTR_BYTE_OFF;
const _: () = assert!(HEADER_SIZE % WORD == 0);

#[derive(Debug, Clone, Copy)]
struct BlockHeadView {
    off: usize,
    is_free: bool,
    next_off: usize,
    data_off: usize,
}

unsafe fn from_block_off(block_off: usize, file_start: *const u8) -> Result<BlockHeadView> {
    unsafe {
        let block_ptr = file_start.add(block_off);

        let is_free = *block_ptr.add(IS_FREE_BYTE_OFF) == 1;

        let next_field_contents: usize = usize::from_le_bytes(
            std::slice::from_raw_parts(block_ptr.add(NEXT_PTR_BYTE_OFF), WORD).try_into()?,
        );
        let data_off = block_off + DATA_PTR_BYTE_OFF;
        return Ok(BlockHeadView {
            off: block_off,
            is_free,
            next_off: next_field_contents,
            data_off,
        });
    }
}

unsafe fn from_data_off(data_off: usize, file_start: *const u8) -> Result<BlockHeadView> {
    unsafe {
        let block_off = data_off - DATA_PTR_BYTE_OFF;
        return from_block_off(block_off, file_start);
    };
}

unsafe fn size(block_ptr: *const u8, file_end: *const u8) -> Result<usize> {
    if file_end < block_ptr {
        return Err(anyhow!("End pointer is before block pointer"));
    }
    Ok(file_end as usize - block_ptr as usize)
}

// Upgrades the loc ptr to a block_ptr by writing a well formed location head
unsafe fn write_new_block(
    off: usize,
    is_free: bool,
    next: usize,
    file_start: *mut u8,
) -> Result<BlockHeadView> {
    let loc = unsafe { file_start.add(off) };

    let new_free_ptr = loc;
    let new_next_ptr = unsafe { loc.add(NEXT_PTR_BYTE_OFF) };
    let new_data_off = off + DATA_PTR_BYTE_OFF;

    // do the write
    unsafe { *new_free_ptr = if is_free { 1u8 } else { 0u8 } };

    let next_ptr_as_slice: &mut [u8] =
        unsafe { std::slice::from_raw_parts_mut(new_next_ptr, WORD) };
    next_ptr_as_slice.copy_from_slice(&usize::to_le_bytes(next));

    Ok(BlockHeadView {
        off,            /* that's were we wrote the data */
        is_free,        /* wrote that data */
        next_off: next, /* we wrote that data */
        data_off: new_data_off,
    })
}

unsafe fn next_from_block(
    block_off: usize,
    file_start: *const u8,
) -> Result<Option<BlockHeadView>> {
    let cur = unsafe { from_block_off(block_off, file_start)? };
    if cur.next_off == 0 {
        return Ok(None);
    }
    Ok(Some(unsafe { from_block_off(cur.next_off, file_start) }?))
}

unsafe fn set_free_flag(block_off: usize, is_free: bool, file_start: *mut u8) -> Result<()> {
    unsafe { *(file_start.add(block_off + IS_FREE_BYTE_OFF)) = if is_free { 1u8 } else { 0u8 } };
    Ok(())
}

unsafe fn set_next_off(block_off: usize, next_off: usize, file_start: *mut u8) -> Result<()> {
    let next_ptr_loc = unsafe { file_start.add(block_off + NEXT_PTR_BYTE_OFF) };
    let next_ptr_loc_as_slice: &mut [u8] =
        unsafe { std::slice::from_raw_parts_mut(next_ptr_loc, WORD) };
    next_ptr_loc_as_slice.copy_from_slice(&usize::to_le_bytes(next_off));
    Ok(())
}

fn check_alignment_is_ok(ptr: *const u8) -> Result<()> {
    if (ptr as usize) % WORD != 0 {
        return Err(anyhow!(
            "Starting pointer must be {}-byte aligned, but it's at address {:p} (alignment {})",
            WORD,
            ptr,
            (ptr as usize) % WORD
        ));
    }
    Ok(())
}

fn align_up(n: usize, alignment: usize) -> usize {
    (n + alignment - 1) & !(alignment - 1)
}

pub unsafe fn init(file_start: *mut u8) -> Result<()> {
    check_alignment_is_ok(file_start)?;
    unsafe { write_new_block(0, true, 0, file_start) }?;
    Ok(())
}

// we are going to rely on unallocated memory being zeros...
pub unsafe fn aloc(n: usize, file_start: *mut u8, file_end: *const u8) -> Result<usize> {
    // scan through all the blocks until we find either:
    //  1) one set to free of sufficent size
    //  2) one with nullptr next_ptr with enough space at the end of the file

    if n == 0 {
        return Err(anyhow!(
            "number of bytes must be greater than zero, received {}",
            n
        ));
    }

    // for sanity, check if file_start is aligned
    check_alignment_is_ok(file_start)?;

    // at the very top we can modify n such that it is well aligned, and then run the normal routine
    // this will allocate some extra space invisibly, but the contract with the caller is that atleast (!) n bytes become available
    // if they make a write of size n, and their implementation writes some more data for alignment, it's okay, since we've set the alignment correctly here.
    let n = align_up(n, WORD);

    let mut cur_block = Some(unsafe { from_block_off(0, file_start) }?);
    while let Some(cur) = cur_block {
        // get the size to the next block or the end of the file
        let size: usize = {
            let ptr = if cur.next_off == 0 {
                file_end
            } else {
                unsafe { file_start.add(cur.next_off) }
            };
            unsafe { size(file_start.add(cur.off), ptr)? }
        };

        // check if this block fits the allocation
        // adding HEADER_SIZE because we need space for
        //  1) the header we are going to write
        if size > (n + HEADER_SIZE) && cur.is_free {
            // fits and it is free.
            // let new_bloc_loc = unsafe { cur.data_ptr.add(n) as *mut u8 };
            let new_block_off = cur.data_off + n;

            unsafe { write_new_block(new_block_off, true, cur.next_off, file_start) }?;
            unsafe { set_free_flag(cur.off, false, file_start) }?;
            unsafe { set_next_off(cur.off, new_block_off, file_start) }?;
            // we wrote a block at the end of our newly allocated memory
            // we marked this block as not free
            // we wired up this block to point to the new block
            // we are done, return the data_ptr of the cur block!
            return Ok(cur.data_off);
        }

        // walk the list if we don't find a fitting region
        cur_block = unsafe { next_from_block(cur.off, file_start)? };
    }

    // We are here because we exhausted the list, this means there is no space :(
    Err(anyhow!(
        "Insuficent remaining space to allocate {} bytes in file with total size {} bytes",
        n,
        unsafe { size(file_start, file_end)? }
    ))
}

pub unsafe fn dealoc(off: usize, file_start: *mut u8, file_end: *const u8) -> Result<()> {
    let mut block = unsafe { from_data_off(off, file_start) }?;
    unsafe { set_free_flag(block.off, true, file_start) }?;
    block.is_free = true; /* keep our view in sync. */

    // marking the block as free is all we need to do for `alloc` to find this memory again
    // however just doing that would keep the memory extremly fragmented, so we will scan to
    // the right and coalece all blocks that are free into one big memory region.

    // let's first zero the memory we just freed
    unsafe {
        let end = if block.next_off == 0 {
            file_end
        } else {
            file_start.add(block.next_off)
        };
        std::ptr::write_bytes(
            file_start.add(block.data_off),
            0,
            size(file_start.add(block.data_off), end)?,
        )
    };

    // coalecing to the left from the beginning of the file
    let mut cur = unsafe { from_block_off(0, file_start) }?;
    loop {
        // try to get the next block, or break the loop if there isn't one
        let Some(next) = unsafe { next_from_block(cur.off, file_start) }? else {
            break;
        };

        if cur.is_free && next.is_free {
            // merge the two blocks if they are both free (by wriring `cur` to the block one after `next`, skipping `next`)
            unsafe { set_next_off(cur.off, next.next_off, file_start) }?;
            // let's also zero the header we just skipped.
            unsafe {
                std::ptr::write_bytes(file_start.add(next.off), 0, HEADER_SIZE);
            };

            continue; // don't update cur, there might be more blocks on the right to eat.
        }

        // walk the list
        cur = next;
    }

    Ok(())
}

// fn print_memory(memory: *const u8, offset: usize, n: usize) {
//     println!("{:?}", unsafe {
//         std::slice::from_raw_parts(memory.add(offset), n)
//     })
// }

// fn main() -> Result<()> {
//     let memory_size = 4096;
//     let memory = unsafe {
//         // Allocate aligned memory
//         let layout = std::alloc::Layout::from_size_align(memory_size, 8)?;
//         let ptr = std::alloc::alloc_zeroed(layout);
//         if ptr.is_null() {
//             return Err(anyhow!("Failed to allocate memory"));
//         }
//         ptr
//     };
//     let memory_end = unsafe { memory.add(memory_size) };

//     // Some test
//     unsafe {
//         ll_aloc::init(memory)?;
//         print_memory(memory, 0, 20);
//         let ptr = ll_aloc::aloc(4, memory, memory_end)? as *mut u32;
//         *ptr = 69;
//         print_memory(memory, 0, 50);
//         let ptr_2 = ll_aloc::aloc(4, memory, memory_end)? as *mut u32;
//         *ptr_2 = 42;
//         print_memory(memory, 0, 50);
//         ll_aloc::dealoc(ptr_2 as *mut u8, memory_end)?;
//         print_memory(memory, 0, 50);
//     };

//     Ok(())
// }

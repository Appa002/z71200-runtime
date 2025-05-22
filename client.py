import os
import socket
import json
import struct
import ctypes
import ctypes.util
import mmap
import sys
from time import sleep

IPSUM = """Lorem ipsum dolor sit amet, consectetur adipiscing elit. Curabitur consectetur suscipit mollis. Donec condimentum aliquam enim ac vulputate. Donec rutrum malesuada ligula, vitae ornare sapien ultrices quis. Duis accumsan, odio eget convallis vehicula, enim purus fermentum risus, at facilisis metus magna eu metus. Phasellus ultricies accumsan nisl, eget finibus ex facilisis sed. Fusce pellentesque, sem nec porttitor porttitor, augue leo consequat neque, quis pulvinar nunc nunc finibus tortor. Nulla eu blandit arcu. Etiam pretium porttitor faucibus. Vivamus suscipit ultricies purus, sit amet dapibus sem sagittis in. Praesent faucibus auctor commodo. Aliquam auctor lectus sapien, ac hendrerit ipsum molestie vel. Duis vestibulum interdum laoreet. Praesent cursus interdum elit sed pretium. Maecenas sed ex quis ipsum lacinia dapibus a sed nulla. Suspendisse luctus massa sed egestas molestie. Sed porttitor vitae metus non faucibus.

Sed sem libero, posuere sit amet pharetra sed, dignissim a eros. Vestibulum ante ipsum primis in faucibus orci luctus et ultrices posuere cubilia curae; Nam fringilla molestie iaculis. Etiam ut laoreet dui. Vestibulum volutpat laoreet mi at mollis. Vivamus ante ante, luctus nec vehicula in, convallis sit amet metus. Sed tincidunt viverra risus, ut aliquam augue suscipit at. Suspendisse molestie bibendum fringilla. Duis vel efficitur metus.

Pellentesque accumsan dolor enim, quis placerat felis volutpat a. Nulla nec aliquam felis, vel porttitor nunc. Mauris sit amet tincidunt mauris, in bibendum ligula. Nulla facilisi. Quisque pellentesque justo ac ultricies venenatis. Morbi ligula lacus, faucibus at lectus ut, mollis ultrices turpis. Donec erat orci, luctus at mollis ut, vestibulum a neque. Donec vulputate odio id mi ullamcorper mattis. Integer sed laoreet ex. Aenean non quam vulputate, placerat enim posuere, efficitur nisl. Sed nibh massa, viverra non lacinia quis, consequat ut purus.

Class aptent taciti sociosqu ad litora torquent per conubia nostra, per inceptos himenaeos. Phasellus sodales sed dolor ut varius. Integer sit amet faucibus ipsum. Praesent leo ligula, elementum eget nisl id, consectetur volutpat eros. Sed facilisis orci vitae est aliquet finibus. Cras eleifend nisi vel magna viverra, vel dictum dui rutrum. Praesent condimentum, erat ac luctus congue, ipsum lacus aliquam ex, vitae bibendum nunc felis vel quam. Aliquam feugiat gravida felis, luctus pharetra ex pretium eu. Phasellus non velit blandit, commodo lorem ac, porttitor magna. Praesent consectetur commodo porta. Donec auctor arcu quam, ut dictum enim egestas eget. Class aptent taciti sociosqu ad litora torquent per conubia nostra, per inceptos himenaeos. Pellentesque at aliquam augue. Morbi iaculis sollicitudin turpis, egestas vehicula leo vestibulum ac.

Fusce vel urna semper, tincidunt lectus congue, condimentum urna. Ut et auctor dolor, vitae maximus erat. Donec aliquet viverra ipsum, eget viverra tortor ultrices et. Proin eros purus, tincidunt vitae interdum sit amet, auctor eu sem. Interdum et malesuada fames ac ante ipsum primis in faucibus. Nullam pretium pharetra ipsum, a interdum nunc semper dictum. Pellentesque habitant morbi tristique senectus et netus et malesuada fames ac turpis egestas. Maecenas sit amet iaculis ligula. In ultricies urna sed lectus mattis bibendum. Integer finibus cursus erat, at iaculis sem posuere sed.
"""

# Preamble
EXPECTED_PROTOCOL = 1

print("Initializing")
print("Protocol Ver:", os.environ["z71200_PROTOCOL_VERSION"])
print("SHM File:", os.environ["z71200_SHM"])
print("SEM Lock:", os.environ["z71200_SEM_LOCK"])
print("SEM Ready:", os.environ["z71200_SEM_READY"])
print("Sock Name:", os.environ["z71200_SOCK"])

assert os.environ["z71200_PROTOCOL_VERSION"] == str(EXPECTED_PROTOCOL)

MACHINE_WORD = (sys.maxsize.bit_length() + 1) // 8

# Client
def _open_sem(path, libc):
    sem = libc.sem_open(path.encode(), os.O_RDWR)
    if not sem:
        raise Exception(f"sem_open failed: {ctypes.get_errno()}")
    return sem

def _open_shared_memory(path, libc):
    S_IRUSR   = 0o400
    S_IWUSR   = 0o200
    fd = libc.shm_open(path.encode(), os.O_RDWR, S_IRUSR | S_IWUSR)
    if fd < 0:
        raise Exception(f"shm_open failed: {os.strerror(ctypes.get_errno())}")
    return fd

def _sem_wait(sem, libc):
    if libc.sem_wait(sem) != 0:
        raise Exception(f"sem_post failed: {os.strerror(ctypes.get_errno())}")

def _sem_post(sem, libc):
    if libc.sem_post(sem) != 0:
        raise Exception(f"sem_post failed: {os.strerror(ctypes.get_errno())}")


class Z71200Context:
    def __init__(self) -> None:
        # Socket communication
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(os.environ["z71200_SOCK"])
        # Open libc and setup interop
        libc_path = ctypes.util.find_library("c")
        if libc_path is None: raise Exception("C Library not found")
        self.libc = ctypes.CDLL(libc_path, use_errno=True)
        ## Set return types
        for _fn in ("shm_open", "sem_open", "sem_post", "sem_close", "sem_wait"):
            getattr(self.libc, _fn).restype = ctypes.c_int
        self.libc.sem_open.restype = ctypes.POINTER(ctypes.c_void_p)

        ##  Open semaphores and mmaped file and get
        self.sem_ready = _open_sem(os.environ["z71200_SEM_READY"], self.libc)
        self.sem_lock = _open_sem(os.environ["z71200_SEM_LOCK"], self.libc)

        # Open shared file
        ## (it is important that the mmap object and buf object are stored on this class to avoid GC cleaning it up)
        LEN = 1_024
        fd = _open_shared_memory(os.environ["z71200_SHM"], self.libc)
        self.mm = mmap.mmap(fd, LEN, mmap.MAP_SHARED, mmap.PROT_READ | mmap.PROT_WRITE)
        self.buf = (ctypes.c_char * LEN).from_buffer(self.mm)
        self.shm_base = ctypes.addressof(self.buf)

        # Get pointers into mmaped file
        VERSION_OFF = 0 # 0 bit
        DATA_OFF = VERSION_OFF + 8 # 64 bit
        self.version_ptr = ctypes.cast(self.shm_base + VERSION_OFF, ctypes.POINTER(ctypes.c_uint64))
        self.data_ptr = ctypes.cast(self.shm_base + DATA_OFF, ctypes.POINTER(ctypes.c_uint8))

    def unsafe_read_version(self): # unsafe because assumes lock is held
        return struct.unpack("<I", ctypes.string_at(ctypes.addressof(self.version_ptr.contents), 4))[0]

    def safe_write(self, data, loc):
        assert isinstance(data, (bytes, bytearray, memoryview))
        _sem_wait(self.sem_lock, self.libc)

        assert self.unsafe_read_version() == EXPECTED_PROTOCOL

        for i, b in enumerate(data):
            self.data_ptr[loc + i] = b

        _sem_post(self.sem_lock, self.libc)
        _sem_post(self.sem_ready, self.libc)

    def redraw(self):
        _sem_post(self.sem_ready, self.libc)

    def safe_read(self, loc, n):
        _sem_wait(self.sem_lock, self.libc)
        assert self.unsafe_read_version() == EXPECTED_PROTOCOL

        buf = bytearray(n)
        for i in range(n):
            buf[i] = self.data_ptr[loc + i]


        _sem_post(self.sem_lock, self.libc)
        return bytes(buf)

    def recv_exact(self, size):
        buffer = b''
        remaining = size
        while remaining > 0:
            chunk = self.sock.recv(min(remaining, 4096))
            if not chunk: raise Exception("Socket hungup too soon.")
            buffer += chunk
            remaining -= len(chunk)
        return buffer

    def send(self, obj):
        payload = json.dumps(obj).encode("utf-8")
        size = struct.pack('<I', len(payload)) # u32 little-endian bytes
        self.sock.sendall(size + payload)

    def ask(self, obj):
        # send a message & block until response is received
        # an obj of type ask is garuanteed by the system to receive a response
        # before anything else on the socket. So after sending a `ask` type obj
        # you are guaranteed that the next thing on the socket is the response.
        assert obj["kind"] == "ask"
        self.send(obj)
        return self.recv()

    def recv(self): # block waiting for msgs
        size = struct.unpack('<I', self.recv_exact(4))[0] # little-endian u32 indicating message size
        return json.loads(self.recv_exact(size).decode('utf-8'))


#########
def print_memory(ptr, n_bytes, bytes_per_row=32):
    """
    Print n_bytes of memory from a pointer in a readable format with offsets from start
    and hexadecimal and ASCII representation.

    Args:
        ptr: A ctypes pointer to the memory location to print
        n_bytes: Number of bytes to print
        bytes_per_row: Number of bytes to display per row (default 32)
    """
    # Cast the pointer to an array of unsigned bytes
    byte_array = ctypes.cast(ptr, ctypes.POINTER(ctypes.c_uint8))


    # Print header
    print(f"{'Offset':12} | {'Hexadecimal':{bytes_per_row*3}} | {'ASCII':{bytes_per_row}}")
    print("-" * (12 + 3 + bytes_per_row*3 + 3 + bytes_per_row))

    # Print rows
    for i in range(0, n_bytes, bytes_per_row):
        # Calculate how many bytes to print in this row (handle last row)
        bytes_in_row = min(bytes_per_row, n_bytes - i)

        # Offset from start (instead of absolute address)
        offset_str = f"{i:04}"

        # Hex values
        hex_values = []
        ascii_chars = []

        for j in range(bytes_in_row):
            byte_value = byte_array[i + j]
            if byte_value == 0:
                hex_values.append(f"{byte_value:02x}")
            else:
                hex_values.append(f"\033[0;31m{byte_value:02x}\033[0m")

            # Convert to ASCII (printable characters only)
            if 32 <= byte_value <= 126:  # Printable ASCII range
                ascii_chars.append(chr(byte_value))
            else:
                ascii_chars.append('.')

        # Pad hex values if needed
        hex_str = ' '.join(hex_values).ljust(bytes_per_row * 3)
        ascii_str = ''.join(ascii_chars).ljust(bytes_per_row)

        # Print the row
        print(f"{offset_str:12} | {hex_str} | {ascii_str}")

####


# Std library
ctx = Z71200Context()


def into_ask(name, **args):
    resp = ctx.ask({'kind': 'ask', 'fn': name, 'args': args});
    if resp['kind'] == 'error': raise Exception(resp['error'])
    return resp['return']

def aloc(n): return into_ask("aloc", n=n)
def dealoc(ptr): return into_ask("dealoc", ptr=ptr)
def set_root(ptr): return into_ask("set_root", ptr=ptr)

def write_tagged_word(ptr, tag, word):
    if word is None: word = 0xdeadbeefb00bee30
    if isinstance(word, int): word = word.to_bytes(MACHINE_WORD, byteorder='little', signed=False)
    if isinstance(word, float):
        word = struct.pack('<f', word)
        if len(word) < MACHINE_WORD: word = word + b'\x00' * (MACHINE_WORD - len(word))

    assert isinstance(word, bytes)
    assert len(word) == MACHINE_WORD

    tag = tag.to_bytes(MACHINE_WORD, byteorder='little', signed=False)
    ctx.safe_write(tag + word, ptr)
    return ptr + MACHINE_WORD * 2

def aloc_tagged_str(str: str):
    bytes = str.encode("utf-8")
    ptr = aloc(len(bytes) + 2*MACHINE_WORD)
    write_tagged_word(ptr, 0, len(bytes)) # Array (length) at ptr
    ctx.safe_write(bytes, ptr + 2*MACHINE_WORD)
    return ptr

sleep(.5)

# str = aloc_tagged_str(IPSUM);
# root = aloc(2 * MACHINE_WORD * 64)
# cursor = root
# cursor = write_tagged_word(cursor, 9, None) # Enter (root)
# cursor = write_tagged_word(cursor, 23, None) # Padding
# cursor = write_tagged_word(cursor, 1, 10.0) # Pxs
# cursor = write_tagged_word(cursor, 1, 10.0) # Pxs
# cursor = write_tagged_word(cursor, 1, 10.0) # Pxs
# cursor = write_tagged_word(cursor, 1, 10.0) # Pxs

# cursor = write_tagged_word(cursor, 32, 1) # Library (button)

# cursor = write_tagged_word(cursor, 10, None) # Leave (root)

# set_root(root)
#




str = aloc_tagged_str(IPSUM);
root = aloc(2 * MACHINE_WORD * 64)
cursor = root
# Layout
cursor = write_tagged_word(cursor, 9, None) # Enter (root)
cursor = write_tagged_word(cursor, 21, None)# Width
cursor = write_tagged_word(cursor, 3, 1.0) # Frac, 1.0
cursor = write_tagged_word(cursor, 22, None)# Height
cursor = write_tagged_word(cursor, 3, 1.0) # Frac, 1.0
# Text
#
cursor = write_tagged_word(cursor, 41, None) #Text, x, y, ptr
cursor = write_tagged_word(cursor, 1, 0) # Pxs, 0
cursor = write_tagged_word(cursor, 1, 0) # Pxs 0
cursor = write_tagged_word(cursor, 42, str) # TextPtr <ptr>

cursor = write_tagged_word(cursor, 10, None) # Leave (root)

    # Frac, /* 3 */


set_root(root)

while True:
    msg = ctx.recv()
    print(msg);
    sleep(1./1000. * 5)

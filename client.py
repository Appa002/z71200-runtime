from collections.abc import Iterable
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
    if isinstance(word, bytes) and len(word) < MACHINE_WORD:
        word = word + b'\x00' * (MACHINE_WORD - len(word))

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

# Drawing
def rgb(str): return ('rgb', bytes.fromhex(str))
def rgba(str): return ('rgba', bytes.fromhex(str))
def hsv(str): return ('hsv', bytes.fromhex(str))
def hsva(str): return ('hsva', bytes.fromhex(str))
def write_color(cursor, v):
    if v[0] == "rgb": return write_tagged_word(cursor, 5, v[1])
    elif v[0] == "rgba": return write_tagged_word(cursor, 7, v[1])
    elif v[0] == "hsv": return write_tagged_word(cursor, 6, v[1])
    elif v[0] == "hsva": return write_tagged_word(cursor, 8, v[1])
    else: raise Exception("Unknown value for color.")

def pxs(v): return ('pxs', v)
def rems(v): return ('rems', v)
def frac(v): return ('frac', v)
def auto(): return ('auto', None)
def write_length(cursor, v):
    if v[0] == 'pxs': return  write_tagged_word(cursor, 1, float(v[1]))
    elif v[0] == 'rems': return  write_tagged_word(cursor, 2, float(v[1]))
    elif v[0] == 'frac': return  write_tagged_word(cursor, 3, float(v[1]))
    elif v[0] == 'auto': return  write_tagged_word(cursor, 4, None)
    else: raise Exception("Unknown value for length parameter.")

def write_either_literal(cursor, v):
    if v[0] in ['rgb', 'rgba', 'hsv', 'hsva']: return write_color(cursor, v)
    if v[0] in ['pxs', 'rems', 'frac', 'auto']: return write_length(cursor, v)
    else: raise Exception("Unknown value for length parameter.")

def color(c):
    def f(cursor):
        cursor = write_tagged_word(cursor, 21, None)
        return write_color(cursor, c)
    return f
def rect(x, y, width, height):
    def f(cursor):
        cursor = write_tagged_word(cursor, 11, None)
        cursor = write_length(cursor, x)
        cursor = write_length(cursor, y)
        cursor = write_length(cursor, width)
        return write_length(cursor, height)
    return f
def rounded_rect( x, y, width, height, r):
    def f(cursor):
        cursor = write_tagged_word(cursor, 12, None)
        cursor = write_length(cursor, x)
        cursor = write_length(cursor, y)
        cursor = write_length(cursor, width)
        cursor = write_length(cursor, height)
        return write_length(cursor, r)
    return f

## Path
def begin_path(): return lambda cursor: write_tagged_word(cursor, 13, None)
def end_path(): return lambda cursor: write_tagged_word(cursor, 14, None)
def move_to(x, y):
    def f(cursor):
        cursor = write_tagged_word(cursor, 15, None)
        cursor = write_length(cursor, x)
        return write_length(cursor, y)
    return f
def line_to(x, y):
    def f(cursor):
        cursor = write_tagged_word(cursor, 16, None)
        cursor = write_length(cursor, x)
        return write_length(cursor, y)
    return f
def quad_to(cx, cy, x, y):
    def f(cursor):
        cursor = write_tagged_word(cursor, 17, None)
        cursor = write_length(cursor, cx)
        cursor = write_length(cursor, cy)
        cursor = write_length(cursor, x)
        return write_length(cursor, y)
    return f
def cubic_to(cursor, cx1, cy1, cx2, cy2, x, y):
    def f(cursor):
        cursor = write_tagged_word(cursor, 18, None)
        cursor = write_length(cursor, cx1)
        cursor = write_length(cursor, cy1)
        cursor = write_length(cursor, cx2)
        cursor = write_length(cursor, cy2)
        cursor = write_length(cursor, x)
        return write_length(cursor, y)
    return  f
def arc_to(tx, ty, x, y, r):
    def f(cursor):
        cursor = write_tagged_word(cursor, 19, None)
        cursor = write_length(cursor, tx)
        cursor = write_length(cursor, ty)
        cursor = write_length(cursor, x)
        cursor = write_length(cursor, y)
        return write_length(cursor, r)
    return f
def close_path(): return lambda cursor: write_tagged_word(cursor, 20, None)

# Layout
def width(w):
    def f(cursor):
        cursor = write_tagged_word(cursor, 22, None)
        return write_length(cursor, w)
    return f
def height(w):
    def f(cursor):
        cursor = write_tagged_word(cursor, 23, None)
        return write_length(cursor, w)
    return f

def padding(left, top, right, bottom):
    def f(cursor):
        cursor = write_tagged_word(cursor, 24, None)
        cursor = write_length(cursor, left)
        cursor = write_length(cursor, top)
        cursor = write_length(cursor, right)
        return write_length(cursor, bottom)
    return f
def margin(left, top, right, bottom):
    def f(cursor):
        cursor = write_tagged_word(cursor, 25, None)
        cursor = write_length(cursor, left)
        cursor = write_length(cursor, top)
        cursor = write_length(cursor, right)
        return write_length(cursor, bottom)
    return f
def display(display_option): return lambda cursor: write_tagged_word(cursor, 26, display_option)
def gap(g):
    def f(cursor):
        cursor = write_tagged_word(cursor, 27, None)
        return write_length(cursor, g)
    return  f

# States
# def hover( rel_pointer): return write_tagged_word(cursor, 28, rel_pointer)
# def mouse_pressed(cursor, rel_pointer): return write_tagged_word(cursor, 29, rel_pointer)
# def clicked(cursor, rel_pointer): return write_tagged_word(cursor, 30, rel_pointer)
# def open_latch(cursor, rel_pointer): return write_tagged_word(cursor, 31, rel_pointer)
# def closed_latch(cursor, rel_pointer): return write_tagged_word(cursor, 32, rel_pointer)
# def push_arg(cursor, arg): return write_tagged_word(cursor, 33, arg)
# def pull_arg(cursor): return write_tagged_word(cursor, 34, None)
# def pull_arg_or(cursor, default_f):
#     cursor = write_tagged_word(cursor, 35, None)
#     return default_f(cursor)
# def load_reg(cursor, word): return write_tagged_word(cursor, 36, word)
# def from_reg(cursor, word): return write_tagged_word(cursor, 37, word)
# def from_reg_or(cursor, word, default_f):
#     cursor = write_tagged_word(cursor, 38, word)
#     return default_f(cursor)
# def event(cursor, id): return write_tagged_word(cursor, 39, id)
# def cursor_default(cursor): return write_tagged_word(cursor, 45, None)
# def cursor_pointer(cursor): return write_tagged_word(cursor, 46, None)

# Text
def text(x, y, ptr):
    def f(cursor):
        cursor = write_tagged_word(cursor, 40, None)
        cursor = write_length(cursor, x)
        cursor = write_length(cursor, y)
        cursor = write_tagged_word(cursor, 41, ptr)
    return f
def font_size( size): return lambda cursor: write_tagged_word(cursor, 42, float(size))
def font_alignment( alignment): return lambda cursor: write_tagged_word(cursor, 43, alignment)
def font_family(ptr):
    def f(cursor):
        cursor = write_tagged_word(cursor, 44, None)
        return write_tagged_word(cursor, 41, ptr)
    return f


# >>  Higher Level Components
## Event map and generic handler
GLOBAL_CALLBACK_MAP = {}
def handle_event(obj):
    id = obj.get('evt_id', None)
    if id is None: return;
    if id not in GLOBAL_CALLBACK_MAP: return;
    GLOBAL_CALLBACK_MAP[id]()

## Deal with event modification
def write_cond_evt(cursor, fn, tag):
    cursor = write_tagged_word(cursor, tag, MACHINE_WORD * 2)
    cursor = write_tagged_word(cursor, 39, len(GLOBAL_CALLBACK_MAP))
    GLOBAL_CALLBACK_MAP[len(GLOBAL_CALLBACK_MAP)] = fn
    return cursor
## Deal with style modification
def _branch(cursor, f, tag):
    cursor = write_tagged_word(cursor, tag, MACHINE_WORD * 2 * 2)
    cursor = f(cursor)
    return cursor
def _branch_w_default(cursor, f, d_f, tag):
    cursor = write_tagged_word(cursor, tag, MACHINE_WORD * 2 * 3)
    cursor = f(cursor)
    cursor = write_tagged_word(cursor, 32, MACHINE_WORD * 2 * 2)
    return d_f(cursor)
def write_cond_style(cursor, v, style_f): # v is either ('rgb', bytes) or ('hover', ('rgb', bytes)) or ('hover', ('rgb', bytes), ('rgb', bytes))
    if not isinstance(v[1], tuple): return style_f(v)(cursor)
    if len(v) == 2: #nodefault
        if v[0] == 'hover':   return _branch(cursor, style_f(v[1]), 28)
        if v[0] == 'pressed': return _branch(cursor, style_f(v[1]), 29)
        if v[0] == 'clicked': return _branch(cursor, style_f(v[1]), 30)
        raise Exception("Unknown conditional state in", v)
    if len(v) == 3: #w/default
        if v[0] == 'hover':   return _branch_w_default(cursor, style_f(v[1]), style_f(v[2]), 28)
        if v[0] == 'pressed': return _branch_w_default(cursor, style_f(v[1]), style_f(v[2]), 29)
        if v[0] == 'clicked': return _branch_w_default(cursor, style_f(v[1]), style_f(v[2]), 30)
        raise Exception("Unknown conditional state in", v)
    raise Exception('Bad conditional format', v)

## Component
def div(children, w=auto(), h=auto(), r=pxs(0), bg=rgb('cccccc'), clicked=None, hover=None, pressed=None):
    def f(cursor):
        # Layout
        cursor = write_tagged_word(cursor, 9, None) # Enter
        cursor = width(w)(cursor)
        cursor = height(h)(cursor)

        # Draw
        cursor = write_cond_style(cursor, bg, color)
        cursor = rounded_rect(pxs(0), pxs(0), w, h, r)(cursor)

        # Events
        if clicked is not None: cursor = write_cond_evt(cursor, clicked, 30)
        if hover is not None: cursor = write_cond_evt(cursor, hover, 28)
        if pressed is not None: cursor = write_cond_evt(cursor, pressed, 29)

        for c in children: cursor = c(cursor)

        cursor = write_tagged_word(cursor, 10, None) # Leave
        return cursor
    return f


# Final
def inflate(loc, root):
    root(loc)
    set_root(loc)
    ctx.redraw()


## Buisness Logic
str = aloc_tagged_str(IPSUM);
root = aloc(2 * MACHINE_WORD * 64)

def f(): print("hello, world");

inflate(root,
    div([], w=pxs(200), h=pxs(200), bg=('clicked', rgb('ff0000'), rgb('cccccc')) ,clicked=f)
)

while True:
    msg = ctx.recv()
    handle_event(msg)
    sleep(1./1000. * 5)

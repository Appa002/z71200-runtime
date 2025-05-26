import os
import socket
import json
import struct
import ctypes
import ctypes.util
import mmap
import sys
from time import sleep

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
def write_width(cursor, w):
        cursor = write_tagged_word(cursor, 22, None)
        return write_length(cursor, w)
def write_height(cursor, h):
    cursor = write_tagged_word(cursor, 23, None)
    return write_length(cursor, h)

def write_padding(cursor, left, top, right, bottom):
    cursor = write_tagged_word(cursor, 24, None)
    cursor = write_length(cursor, left)
    cursor = write_length(cursor, top)
    cursor = write_length(cursor, right)
    return write_length(cursor, bottom)
def write_margin(cursor, left, top, right, bottom):
    cursor = write_tagged_word(cursor, 25, None)
    cursor = write_length(cursor, left)
    cursor = write_length(cursor, top)
    cursor = write_length(cursor, right)
    return write_length(cursor, bottom)
def write_display(cursor, display_option): return write_tagged_word(cursor, 26, display_option)
def write_gap(cursor, gw, gh):
    cursor = write_tagged_word(cursor, 27, None)
    cursor = write_length(cursor, gw)
    cursor = write_length(cursor, gh)
    return cursor

# Mouse
def cursor_default(): return ('cursor', 'default')
def cursor_pointer(): return ('cursor', 'pointer')
def mouse_cursor(c):
    def f(cursor):
        if c[1] == "default":
           return write_tagged_word(cursor, 45, None)
        elif c[1] == "pointer":
            return  write_tagged_word(cursor, 46, None)
        raise Exception("Unknown cursor type", c)
    return f

# Text
def write_font_size(cursor, size): return write_tagged_word(cursor, 42, float(size))
def write_font_alignment(cursor, alignment): return write_tagged_word(cursor, 43, alignment)
def write_font_family(cursor, ptr):
    cursor = write_tagged_word(cursor, 44, None)
    return write_tagged_word(cursor, 41, ptr)


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
def _branch(cursor, f, tag, n):
    cursor = write_tagged_word(cursor, tag, MACHINE_WORD * 2 * n)
    cursor = f(cursor)
    return cursor
def _branch_w_default(cursor, f, d_f, tag, n):
    cursor = write_tagged_word(cursor, tag, MACHINE_WORD * 2 * (n + 1))
    cursor = f(cursor)
    cursor = write_tagged_word(cursor, 32, MACHINE_WORD * 2 * n)
    return d_f(cursor)
def write_cond_style(cursor, v, style_f, n): # v is either ('rgb', bytes) or ('hover', ('rgb', bytes)) or ('hover', ('rgb', bytes), ('rgb', bytes))
    if not isinstance(v[1], tuple): return style_f(v)(cursor)
    if len(v) == 2: #nodefault
        if v[0] == 'hover':   return _branch(cursor, style_f(v[1]), 28, n)
        if v[0] == 'pressed': return _branch(cursor, style_f(v[1]), 29, n)
        if v[0] == 'clicked': return _branch(cursor, style_f(v[1]), 30, n)
        raise Exception("Unknown conditional state in", v)
    if len(v) == 3: #w/default
        if v[0] == 'hover':   return _branch_w_default(cursor, style_f(v[1]), style_f(v[2]), 28, n)
        if v[0] == 'pressed': return _branch_w_default(cursor, style_f(v[1]), style_f(v[2]), 29, n)
        if v[0] == 'clicked': return _branch_w_default(cursor, style_f(v[1]), style_f(v[2]), 30, n)
        raise Exception("Unknown conditional state in", v)
    raise Exception('Bad conditional format', v)

## Component
def div(children,
    w=auto(), h=auto(), r=pxs(0), bg=rgb('cccccc'),
    padding=(pxs(0), pxs(0), pxs(0), pxs(0)),
    margin=(pxs(0), pxs(0), pxs(0), pxs(0)),
    mouse=cursor_default(),
    clicked=None, hover=None, pressed=None
):
    def f(cursor):
        # Layout
        cursor = write_tagged_word(cursor, 9, None) # Enter
        cursor = write_width(cursor, w)
        cursor = write_height(cursor, h)

        cursor = write_padding(cursor, *padding)
        cursor = write_margin(cursor, *margin)

        # Draw
        cursor = write_cond_style(cursor, bg, color, 2)
        cursor = write_cond_style(cursor, mouse, mouse_cursor, 1)
        cursor = rounded_rect(pxs(0), pxs(0), w, h, r)(cursor)

        # Events
        if clicked is not None: cursor = write_cond_evt(cursor, clicked, 30)
        if hover is not None: cursor = write_cond_evt(cursor, hover, 28)
        if pressed is not None: cursor = write_cond_evt(cursor, pressed, 29)

        for c in children: cursor = c(cursor)

        cursor = write_tagged_word(cursor, 10, None) # Leave
        return cursor
    return f

def row(children,
    w=auto(), h=auto(),
    padding=(pxs(0), pxs(0), pxs(0), pxs(0)),
    margin=(pxs(0), pxs(0), pxs(0), pxs(0)),
    gap=pxs(0),
    clicked=None, hover=None, pressed=None
):
    def f(cursor):
        # Layout
        cursor = write_tagged_word(cursor, 9, None) # Enter
        cursor = write_width (cursor, w)
        cursor = write_height(cursor, h)
        cursor = write_display(cursor, 1) # FlexRow

        cursor = write_padding(cursor, *padding)
        cursor = write_margin(cursor, *margin)
        cursor = write_gap(cursor, gap, pxs(0))

        # Events
        if clicked is not None: cursor = write_cond_evt(cursor, clicked, 30)
        if hover is not None: cursor = write_cond_evt(cursor, hover, 28)
        if pressed is not None: cursor = write_cond_evt(cursor, pressed, 29)

        for c in children: cursor = c(cursor)

        cursor = write_tagged_word(cursor, 10, None) # Leave
        return cursor
    return f

def col(children,
    w=auto(), h=auto(),
    padding=(pxs(0), pxs(0), pxs(0), pxs(0)),
    margin=(pxs(0), pxs(0), pxs(0), pxs(0)),
    gap=pxs(0),
    clicked=None, hover=None, pressed=None
):
    def f(cursor):
        # Layout
        cursor = write_tagged_word(cursor, 9, None) # Enter
        cursor = write_width(cursor, w)
        cursor = write_height(cursor, h)
        cursor = write_display(cursor, 2) # FlexRow

        cursor = write_padding(cursor, *padding)
        cursor = write_margin(cursor, *margin)
        cursor = write_gap(cursor, pxs(0), gap)

        # Events
        if clicked is not None: cursor = write_cond_evt(cursor, clicked, 30)
        if hover is not None: cursor = write_cond_evt(cursor, hover, 28)
        if pressed is not None: cursor = write_cond_evt(cursor, pressed, 29)

        for c in children: cursor = c(cursor)

        cursor = write_tagged_word(cursor, 10, None) # Leave
        return cursor
    return f

def span(text_ptr, x=pxs(0), y=pxs(0), w=auto(), text_color=rgb('000000'), alignment="start", size=None, font_family=None):
    def f(cursor):
        # Layout
        cursor = write_tagged_word(cursor, 9, None) # Enter
        cursor = write_width(cursor, w)
        # Draw
        cursor = write_cond_style(cursor, text_color, color, 2)

        if alignment == 'start': cursor = write_font_alignment(cursor, 0)
        elif alignment == 'end': cursor = write_font_alignment(cursor, 1)
        elif alignment == 'left': cursor = write_font_alignment(cursor, 2)
        elif alignment == 'middle': cursor = write_font_alignment(cursor, 3)
        elif alignment == 'right': cursor = write_font_alignment(cursor, 4)
        elif alignment == 'justified': cursor = write_font_alignment(cursor, 5)

        if size is not None: cursor = write_font_size(cursor, size)
        if font_family is not None: cursor = write_font_family(cursor, font_family.str_ptr)

        cursor = write_tagged_word(cursor, 40, None) # write text, x, y
        cursor = write_length(cursor, x)
        cursor = write_length(cursor, y)
        cursor = text_ptr.write_ref(cursor) # writes the ptr

        cursor = write_tagged_word(cursor, 10, None) # Leave
        return cursor
    return f

class TextPtr:
    def __init__(self, text) -> None:
        self.str_ptr = aloc_tagged_str(text)
        self.ref_ptr = None
        self.capacity = len(text.encode('utf-8'))

    def write_ref(self, cursor):
        cursor = write_tagged_word(cursor, 41, self.str_ptr)
        self.ref_ptr = cursor - 2 * MACHINE_WORD
        return cursor

    def update(self, text):
        if self.ref_ptr is None: raise Exception("Can't update managed string if it hasn't been written in memory yet")
        bytes = text.encode("utf-8")
        if len(bytes) > self.capacity: # alocate a new string
            dealoc(self.str_ptr)
            self.str_ptr = aloc_tagged_str(text)
            self.capacity = len(text.encode('utf-8'))
            # update the reference to point to the new string
            ctx.safe_write(self.str_ptr.to_bytes(MACHINE_WORD, byteorder='little', signed=False), self.ref_ptr + MACHINE_WORD)
        else: # it fits
            ctx.safe_write(bytes, self.str_ptr + 2*MACHINE_WORD) # write bytes
            ctx.safe_write(len(bytes).to_bytes(MACHINE_WORD, byteorder='little', signed=False), self.str_ptr + MACHINE_WORD) # write new array size


# Final
def inflate(loc, root):
    root(loc)
    set_root(loc)
    ctx.redraw()


## Example
count = 0
count_text = TextPtr("Clicked 0 times");

root = aloc(2 * MACHINE_WORD * 256)
def f():
    global count
    count += 1
    count_text.update(f"Clicked {count} times")
    ctx.redraw()

inflate(root,
    row([
          div([
              span(TextPtr("Click me!"), w=frac(1.0), alignment="middle", y=pxs(8))
          ], w=pxs(100), h=pxs(30), bg=('clicked', rgb('ff0000'), rgb('cccccc')), mouse=('hover', cursor_pointer()),clicked=f),
          span(count_text, w=pxs(500))
    ], padding=(pxs(10), pxs(10), pxs(10), pxs(10)), gap=pxs(10) )
)

while True:
    msg = ctx.recv()
    handle_event(msg)
    sleep(1./1000. * 5)

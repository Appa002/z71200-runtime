# Introduction

The z71200 runtime at its core is intended as a user interface layouting and rendering backend. The executable launches a child process which communicates with a server to define and react to the interactive interface. The motivation for this project is to be a minimalist take on the role that the HTML + JS web environment often serves.

Many projects use "web technologies" for their user interface needs since they are the simplest to use. Particularly, the many years of javascript framework evolution has resulted in extremely sophisticated solutions that make most of the problems in structuring a UI-heavy application ergonomic. I believe the reason this happened with web technology is not necessarily because "it runs everywhere" but because the DOM sits at the perfect level of abstraction with an imperative API that makes code-structure innovation possible without bogging down the developer in the details of rendering and layout. Said another way, the DOM is bare enough to allow people to innovate in the philosophy of writing UI applications without being so bare as to require anyone to actually think about how layouts and rendering work.

This project hopes to sit at the same level of abstraction but without any of the staggering overhead of the modern web.

Works on all real operating systems (FreeBSD, MacOS, Linux), if someone wishes to contribute Windows support that would be greatly appreciated.

This is **experimental** software born as a spinout from another project, comments on missing features and bug reports are greatly appreciated. Has only been tested on MacOS properly.

# Goals

- Client language agnostic. You are able to write a client (the child process) in any language you wish.
- Client code simplicity. The client should be succinct and only require tools available in most language's standard libraries. (The example python client, client.py, is 500 loc of which 200 is communicating with the server and 300 is a very bare ui library).
- Flexible representation. The layout representation the client interacts with should be flexible enough that one can implement the abstractions appropriate to the client's language on top of it. A client written in Haskell, for instance, has a different natural way of expressing user interfaces than one written in JavaScript, C, or LISP, and we should respect that.
- The retained representation of the layout, its modification, and its transport between the client and server process should be so fast as to never be the bottleneck at interactive (120FPS) speeds. This way the currently naive (and therefore somewhat slow) rendering code can be improved without changing the client-facing API (particularly not changing it to such an extent that libraries developed against it need to rethink their memory representation).
- To that end, zero-copy of the layout written by the client for rendering.

# Todo

- Accessibility. Currently, none of the things rendered on screen are exposed to any accessibility APIs, it is therefore not possible to use these applications with a screen reader or any other kind of aid. An effort must be made to meet reasonable standards for all applications developed with this runtime and also expose the necessary tools to make any application truly accessible.
- "Library Mode." Let other rust crates consume this crate as a library to extend the protocol exposed to the client with further features.
- Animations. The renderer under the hood is Skia, drawing with the standard SVG primitives. It may not be too much complexity to add interpolation of drawing instructions between keyframes to facilitate animations.
- Filters. Skia supports filters, it is only a question of exposing them to the client. This would make it possible to implement drop shadows, for instance.
- Windows Support. Currently, the client communicates via a Unix Socket and a shared memory-mapped file. Both of these are POSIX objects so a shim would need to be written for windows. In principle, everything else should just work.
- Variable page size. Currently, the page allocated for the retained layout is 32kb in size allowing up to 2,000 instructions (on 64-bit). Add a mechanism for multiple pages or larger ones.
- Rich text rendering. Text layout is done through the excellent [Parley](https://crates.io/crates/parley) crate which has support for changing all font properties (like weight or colour) throughout the text or laying out a certain width of blank space to, for instance, render equations. Currently, we support none of this, but it is only a matter of exposing through the API.
- Keyboard handling. Unlike mouse events, keyboard events are not wired up.

# Design System Ambitions

It would be great to develop a GPL-licensed Open Source Design System (like those books published by Apple or Google) which defines the look, feel, and nature of all UI elements. Modern UI design is dominated by derivative work which is also often hard to use and illegible. The open-source community could certainly create the most accessible and exclusive design system, which can also bug the trend of modern flat design. With broad adoption, a GPL license would give open-source software a distinct look, perhaps a future mark of excellence.

It would be trivial to create a meta-programming system in this repository, which allows generating binding to this design system in any language by supplying the syntax for the needed language constructs.

# Usage

Install rust and clone this repository.

Build with `cargo build --release`.

Run with `cargo run --release -- <your command>`, for instance `cargo run --release -- python3 -u client.py` will run the example python client on most machines.

The project uses the Vulkan API as its GPU backend through the [Vulkano](https://vulkano.rs) crate. This means you need to have the Vulkan api installed. On mac this means installing the MoltenVK compatibility layer; see the [Vulkano Github](https://github.com/vulkano-rs/vulkano) for more information.

# Documentation

The following should document the client/server interface. In general, "server" will always refer to this runtime and "client" will always refer to your client code.

## Basic User Interface Structure

This runtime uses a semi-retained mode UI. Your client code writes the layout, described in detail below, into a shared memory region which the server reads and renders.

The basic layout follows the "CSS Flexbox" rules as provided through the [Taffy](https://crates.io/crates/taffy) crate. That is, each element defines its width and height as a concrete pixel value, a fraction, or as "Auto" and a `display` property like `Block`, `FlexRow`, `FlexCol`, `Grid`, or `None`. The position and final dimensions of each element are then defined by resolving these constraints.

The underlying rendering is SVG rendering through Skia via the standard SVG commands plus the added `ArcTangentTo` and `RoundedRect` for your convenience. The rendering is done relative to the top-left edge of the element and can in principle extend beyond the element bounds. That is, it is on you to make sure the issued draw commands are such that they do not exceed the bounds of the element as laid out by taffy. This is done to allow in certain cases to break from the strict layout, for instance when drawing indicators for the connection of multiple elements.

## Protocol (Client/Server Communication)

The protocol works through 4 POSIX objects:

- A Unix Socket. Used for the minimal RPC calls the client needs to make to the server (see below).
- A shared memory (SHM) file. Used for the client to write layout and shared objects (like strings) needed by the UI.
- The "Lock" Semaphore. A POSIX semaphore used to synchronise shared access to the above file.
- The "Ready" Semaphore. A POSIX semaphore used to signal that a new layout has been written and the screen must be refreshed.

### The Unix Socket

The Socket is used for sending one of the three possible RPC calls from the client and to receive event notifications (like `clicked`) from the server. The socket uses a *framed json* protocol (sending json over a Unix socket is super lame, but it saves you the complexity of decoding binary messages and json support is part of most standard libraries). The framed protocol works by first sending the message length as a little-endian unsigned 32-bit integer and then the utf-8 encoded json string directly after.

```
>> (frame start)
little-endian u32 message length
|-------------|
utf-8 encoded json string
< (frame end)
```

The protocol assumes that each frame is always sent completely, i.e. no other data can appear between the message length header and the json string.



#### The Json Message that can be sent to the server

| Name     | Schema                                                       | Description                                                  | Response Object                       |
| -------- | ------------------------------------------------------------ | ------------------------------------------------------------ | ------------------------------------- |
| aloc     | `{"kind": "ask", "fn": "aloc", "args": {"n": <bytes>}}`      | Like libc's `maloc`, allocates n bytes in the shared file and returns a "ptr" (offset from the file start) to the first byte. | `{"kind": "return", "return": <ptr>}` |
| dealoc   | `{"kind": "ask", "fn": "dealoc", "args": {"ptr": <offset>}}` | Dealocates the bytes acquired by "aloc" at the offset "ptr". | `{"kind": "return", "return": null}`  |
| set_root | `{"kind": "ask", "fn": "set_root", "args": {"ptr": <offset>}}` | Indicates that the memory location at `ptr` is the current root for the layout, i.e. the runtime will begin reading at that location to build the layout. | `{"kind": "return", "return": null}`  |

As you can see, the basic structure to send to the server is a payload that indicates the "kind" of the message is possible. The kind "ask," which is the only kind of message you can currently send to the server, then requires the "fn" field, indicating the function name, and the "args" mapping, indicating the arguments. The server responds with an object with field `"kind": "return"`  or `"kind": "error"` containing either the field `return` or `error` with the given information.

#### "ask" messages

As mentioned above, the only "kind" of message you can send to the server is called "ask". The reason to distinguish multiple kinds is that this runtime should be able to be extended with other message kinds sent or received via the socket (indeed this is how I intend to use this). The "ask" kind is however special in that it makes the following guarantee: **the response from the server to a kind "ask" message is always the next message sent via the socket.** That is to say, if you send any json payload with  the field `"kind": "ask"`, the next thing the server will send via the socket is the response, so an object with `"kind": "return"`. This makes implementing "ask" messages from the client very easy, as you don't have to deal with any asynchronous code. The example python client at `client.py` exploits this in the `Z71200Context` class and via the `into_ask` function, which returns a python function you can use for an rpc call like any other.

#### The Json Messages sent to the client from the server

Above we already have seen that the server can send messages like `{"kind": "return", "return": <value>}` in response to "ask" message. The server may also respond with a message like `{"kind": "error", "error": <error string>}` indicating an error when resolving an "ask" message.

The only 3rd message that the client is expected to handle is like `{"kind": "event", "evt_id": <id>}` which is sent when an event is fired. Events are fired by elements, for instance when an element is clicked or hovered, the id used is defined by your layout (see below) and it is on your client code to handle associating them with event handlers. (See line `316-330` in `client.py` for how this can be approached).



### The Shared Memory File

The shared memory file is used to define the layout of the user interface as well as to allocate shared objects (such as strings). You can allocate n bytes using the `aloc` RPC call, or you can manage the memory yourself. The server never writes to the shared file, so if you prefer to implement your own allocator over the raw memory, you are welcome to (see `src/ll_aloc.rs` for inspiration on how to write a very simple linked-list backed alocator).

In general, the ui is defined through a sequence of "TaggedWord" structures which are read sequentially. They are a mixture of assembly-like instructions, typed literals, and nested ui layouts.

#### Tagged Words

The basic structure is the tagged word. That is two `usize` (ie the size of the machine word) sized values next to each other. The first is the "Tag", which indicates how to interpret the next, and the second is the "word". For instance, an Rgb colour on a 64-bit little endian machine would be laid out like this.

```
0: [5] [0] [0] [0] [0] [0] [0] [0] | [255] [0] [0] [ ] [ ] [ ] [ ] [ ]
```

Where each `[_]` is one byte and the `|` indicates the word boundary. As you can see, the tag is 5 (using little endianness) which is the tag for "Rgb" and the right word is the sequence `0xff0000`, so shear red. The empty `[]` are left to indicate that they are ignored  -- ie for this instruction they are just padding.

Some tagged words behave like instructions, expecting multiple other tagged words afterwards as the arguments. For instance, the tag "Color" (21) expects one tagged word directly after as its argument. Such instruction-tagged words often accept multiple tags for the next tagged word. For instance, "Color" accepts any tagged word with tag 5, 6, 7, or 8, which are "Rgb", "Hsv", "Rgba", and "Hsva" respectively. So the following are both valid and both set the Skia pencil colour to red.
```
 0: [21] [0] [0] [0] [0] [0] [0] [0] | [   ] [ ] [ ] [ ] [ ] [ ] [ ] [ ]
16: [ 5] [0] [0] [0] [0] [0] [0] [0] | [255] [0] [0] [ ] [ ] [ ] [ ] [ ]
```

Or, using Hsv.

```
 0: [21] [0] [0] [0] [0] [0] [0] [0] | [ ] [   ] [   ] [ ] [ ] [ ] [ ] [ ]
16: [ 6] [0] [0] [0] [0] [0] [0] [0] | [0] [255] [255] [ ] [ ] [ ] [ ] [ ]
```

This also reveals how the tagged words are useful (they also are useful because they're always aligned to the word boundary so no special care needs to be taken by the client when writing them).

The other kinds of value besides colours are lengths, here the tags are 1, 2, 3, or 4 corresponding to Pxs, Rems, or Frac units or Auto as a literal. These take a little endian f32 as the word, so `5.0` pxs is written as.

```
 0: [1] [0] [0] [0] [0] [0] [0] [0] | [64] [160] [0] [0] [ ] [ ] [ ] [ ]
```

The lengths and colours are tagged words defining values with units, most other tagged words behave like instructions (like "Color" above, i.e. taking a number of tagged words after as arguments), there are a few more special concepts before we can give a table of all tags and their expected layout.

#### Element Boundaries

Element boundaries used for layout purposes use a stack-based approach. You use the "Enter" (9) tagged word to enter a new element and the "Leave" (10) tagged word to leave one. These must balance. Child elements are defined by entering a new element while inside the context of one already. Every time you use "Enter" all the tracked properties (like pencil colour) reset, they form a scope in that sense. There are 6 tagged words you can use to define the layout of elements: Width (22), Height (23), Padding (24), Margin (25), Display (26), and Gap (27), see the table below for their exact form. **The first tagged word in your sequence defining a layout must be "Enter"**. To make an example, defining an element with a width of 150 pxs and a height of 100pxs looks like the following.

```
 0: [ 9] [0] [0] [0] [0] [0] [0] [0] | [  ] [   ] [ ] [ ] [ ] [ ] [ ] [ ]
 
16: [22] [0] [0] [0] [0] [0] [0] [0] | [  ] [   ] [ ] [ ] [ ] [ ] [ ] [ ]
32: [ 1] [0] [0] [0] [0] [0] [0] [0] | [67] [ 22] [0] [0] [ ] [ ] [ ] [ ]

48: [23] [0] [0] [0] [0] [0] [0] [0] | [  ] [   ] [ ] [ ] [ ] [ ] [ ] [ ]
64: [ 1] [0] [0] [0] [0] [0] [0] [0] | [66] [200] [0] [0] [ ] [ ] [ ] [ ]

80: [10] [0] [0] [0] [0] [0] [0] [0] | [  ] [   ] [ ] [ ] [ ] [ ] [ ] [ ]
```

Where I've grouped tagged words that belong together (ie "Width" and its argument in "Pxs").

#### Strings

Strings are the only sort of object that needs to be shared between the client and server outside of the actual layout. The layout is to allocate an "Array" (0) tagged word, where the word is the length of the string and then lay out the utf-8 encoded bytes sequentially in memory after. An example of how to do this is the `aloc_tagged_str` method in `client.py` which given a string returns a pointer to the correct structure.

#### Events and State Jmps

The UI needs to react to events and change its style based on the element state. To facilitate this the "Hover" (28), "MousePressed" (29), and "Clicked" (30) tagged words are used. They each take a relative pointer as their associated word. They work like `jne` like instructions in Assembly language, performing the relative jump if the state is *not*  active. That is the following changes the pencil colour to red, if the element is hovered.

```
 0: [28] [0] [0] [0] [0] [0] [0] [0] | [ 32] [0] [0] [0] [0] [0] [0] [0]
 
16: [21] [0] [0] [0] [0] [0] [0] [0] | [   ] [ ] [ ] [ ] [ ] [ ] [ ] [ ]
32: [ 5] [0] [0] [0] [0] [0] [0] [0] | [255] [0] [0] [ ] [ ] [ ] [ ] [ ]
```

The first tagged word at address `0` is the "Hover" jump with relative address `32`. This means if the element is not hovered, the interpreter jumps 32 bytes forward from the end of that tagged word, that is, to address `48` -- right after the "Rgb" tag.

The others work the same, "MousePressed" doesn't jump if the mouse is being held down over the element, and "Clicked" doesn't jump if the mouse was just released over the element. You can also use the unconditional jump "Jmp" (32) tag and the noop tag "NoJmp" (31) to structure your layout. One way of using these is to change the tag in a tagged word from 32 to 31 or vice-versa depending on the programme state. For instance, when implementing radial buttons, where only one can be pressed, the one that has to be drawn in the pressed state is not jumped over, while the others are. There's no bottleneck writing to memory, so you could do this every frame.

Events work through the "Event" (39) tag, it takes a usize integer as its associated word. Every time the interpreter reads the tag, an event with the given id is sent to the client. To implement a clicked event for instance, you'd use the "Clicked" (30) jump to jump over the "Event" (39) tag unless the element was clicked in that frame.



#### Storing tagged words on the stack or registers

The interpreter actually keeps track of a stack and registers that can be used to store and load arguments like one might in traditional assembly machines. This is actually entirely unnecessary and the expectation is that the client code interpolates repeated arguments in the right places. However, it may be ergonomic to use in few situations. "PushArg" (33) reads the next tagged word and puts it onto the stack. "PullArg" (34) pops one argument from the stack and presents it "in its place". Ie if you write the sequence `Color, PullArg`  the colour will be set to whatever argument is pulled from the stack. This errors if no argument is on the stack, however, you can provide a default via "PullArgOr" (35) which reads the next tagged word and provides it as a default if the stack is empty. The register-based manipulations with "LoadReg" (36), "FromReg" (37), and "FromRegOr" (38) are analogous but they all take an integer word for the register id to reference. There are `usize` many registers.



## Tagged Word Table

| ID   | Name          | Word                | Arg 1      | Arg 2    | Arg 3    | Arg 4  | Arg 5  | Arg 6 |
| ---- | ------------- | ------------------- | ---------- | -------- | -------- | ------ | ------ | ----- |
| 0    | Array         | `usize (size)`      |            |          |          |        |        |       |
| 1    | Pxs           | `f32 (value)`       |            |          |          |        |        |       |
| 2    | Rems          | `f32 (value)`       |            |          |          |        |        |       |
| 3    | Frac          | `f32 (value)`       |            |          |          |        |        |       |
| 4    | Auto          |                     |            |          |          |        |        |       |
| 5    | Rgb           | `[u8; 3] (color)`   |            |          |          |        |        |       |
| 6    | Hsv           | `[u8; 3] (color)`   |            |          |          |        |        |       |
| 7    | Rgba          | `[u8; 4] (color)`   |            |          |          |        |        |       |
| 8    | Hsva          | `[u8; 4] (color)`   |            |          |          |        |        |       |
| 9    | Enter         |                     |            |          |          |        |        |       |
| 10   | Leave         |                     |            |          |          |        |        |       |
| 11   | Rect          |                     | x          | y        | width    | height |        |       |
| 12   | RoundedRect   |                     | x          | y        | width    | height | radius |       |
| 13   | BeginPath     |                     |            |          |          |        |        |       |
| 14   | EndPath       |                     |            |          |          |        |        |       |
| 15   | MoveTo        |                     | x          | y        |          |        |        |       |
| 16   | LineTo        |                     | x          | y        |          |        |        |       |
| 17   | QuadTo        |                     | cx         | cy       | x        | y      |        |       |
| 18   | CubicTo       |                     | cx1        | cy1      | cx2      | cy2    | x      | y     |
| 19   | ArcTo         |                     | tx         | ty       | x        | y      | r      |       |
| 20   | ClosePath     |                     |            |          |          |        |        |       |
| 21   | Color         |                     | color      |          |          |        |        |       |
| 22   | Width         |                     | length     |          |          |        |        |       |
| 23   | Height        |                     | length     |          |          |        |        |       |
| 24   | Padding       |                     | left       | top      | right    | bottom |        |       |
| 25   | Margin        |                     | left       | top      | right    | bottom |        |       |
| 26   | Display       | `usize (display)`   |            |          |          |        |        |       |
| 27   | Gap           |                     | horizontal | vertical |          |        |        |       |
| 28   | Hover         | `usize (rel_ptr)`   |            |          |          |        |        |       |
| 29   | MousePressed  | `usize (rel_ptr)`   |            |          |          |        |        |       |
| 30   | Clicked       | `usize (rel_ptr)`   |            |          |          |        |        |       |
| 31   | NoJmp         | `usize (rel_ptr)`   |            |          |          |        |        |       |
| 32   | Jmp           | `usize (rel_ptr)`   |            |          |          |        |        |       |
| 33   | PushArg       |                     | any        |          |          |        |        |       |
| 34   | PullArg       |                     |            |          |          |        |        |       |
| 35   | PullArgOr     |                     | any        |          |          |        |        |       |
| 36   | LoadReg       | `usize (id)`        | any        |          |          |        |        |       |
| 37   | FromReg       | `usize (id)`        |            |          |          |        |        |       |
| 38   | FromRegOr     | `usize (id)`        | any        |          |          |        |        |       |
| 39   | Event         | `usize (evt_id)`    |            |          |          |        |        |       |
| 40   | Text          |                     | x          | y        | text_ptr |        |        |       |
| 41   | TextPtr       | `usize (ptr)`       |            |          |          |        |        |       |
| 42   | FontSize      | `usize (size)`      |            |          |          |        |        |       |
| 43   | FontAlignment | `usize (alignment)` |            |          |          |        |        |       |
| 44   | FontFamily    |                     | text_ptr   |          |          |        |        |       |
| 45   | CursorDefault |                     |            |          |          |        |        |       |
| 46   | CursorPointer |                     |            |          |          |        |        |       |

The display and the font alignment are their own separate mapping like this.

**Display**

| ID   | Name       |
| ---- | ---------- |
| 0    | Block      |
| 1    | FlexRow    |
| 2    | FlexColumn |
| 3    | Grid       |
| 4    | None       |

**Alignment**

| ID   | Name      |
| ---- | --------- |
| 0    | Start     |
| 1    | End       |
| 2    | Left      |
| 3    | Middle    |
| 4    | Right     |
| 5    | Justified |

The word column shows what the expected data to be stored in the associated word is. The arg columns layout which tagged word(s) need to follow as arguments to the instruction, names like `x`, `y`, `r`,  or `width`  allow any of the "length family" tagged words (ie pxs, rems, frac, or auto) to follow. If the word column is empty, it is ignored during parsing. Note that you must always write `usize` many bytes for both the tag and word, ie the structure is always `2*usize` sized, even if a different type is stored. This makes alignment and safe reading trivial.



## Writing a client

As referenced multiple times the provided `client.py` gives a succinct example implementation. The general approach is to first implement the basic communication via the POSIX objects making sure all the synchronisation guarantees are met, to then implement abstractions over the tagged words, allowing you to more easily write a sequence of them, and to then implement a high-level UI framework, which makes state management and such things ergonomic (this 3rd part is where all the innovations between frameworks like React or Svelte sit). 

The provided client is written in such a way that the code gains abstraction as you scroll down. The first four functions wrap the raw `libc` calls, the `Z71200Context` class implements the binary protocol, providing safe abstractions for writing to the shared file, receiving data from the socket, and making rpc calls. The functions after capture the context and provide abstractions for writing strings and sequences of tagged words into memory, the latter by keeping track of a cursor that is advanced for each written word. The last parts then define a high-level ui framework, that then finally allows you to define a UI tree like:

```python
inflate(root,
    row([
          div([
              span(TextPtr("Click me!"), w=frac(1.0), alignment="middle", y=pxs(8))
          ], w=pxs(100), h=pxs(30), r=pxs(5), bg=('clicked', rgb('ff0000'), rgb('cccccc')), mouse=('hover', cursor_pointer()),clicked=f),
          span(count_text, w=pxs(500))
    ], padding=(pxs(10), pxs(10), pxs(10), pxs(10)), gap=pxs(10) )
)
```

This last part is the most stunted and really only provided as a sketch to give you some ideas on what a framework might look like. The aim of this project is to make all the parts leading up to this as easy and succinct as possible without sacrificing performance so that we can get real ui innovation in all languages.


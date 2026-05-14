# cozyui

A tiny pixel-art terminal emulator that uses `alacritty_terminal` for the PTY,
parser, grid, scrollback, escape sequence handling, and resize plumbing.

The window and renderer are deliberately plain X11 through `x11rb`: the
background PNG and bitmap font are composited into a CPU framebuffer, then sent
to the X server with `PutImage`.

Run it from an X11 session:

```sh
cargo run
```

## todo
* icon theme
* file browser app
* todo app
* statusbar

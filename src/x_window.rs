use std::error::Error;
use std::fs::File;
#[cfg(unix)]
use std::os::fd::OwnedFd;

use memmap2::MmapMut;
use x11rb::connection::Connection;
use x11rb::protocol::shm::{ConnectionExt as ShmConnectionExt, Seg};
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, CreateGCAux, CreateWindowAux,
    EventMask, Gcontext, ImageFormat, PropMode, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

use crate::text_input;
use crate::{Framebuffer, Rect};

pub(crate) struct XWindow {
    pub(crate) conn: XCBConnection,
    pub(crate) window: Window,
    gc: Gcontext,
    depth: u8,
    pub(crate) keyboard: text_input::Keyboard,
    upload_buffer: Vec<u8>,
    shm_image: Option<ShmImage>,
}

struct ShmImage {
    seg: Seg,
    mmap: MmapMut,
}

impl XWindow {
    pub(crate) fn open(width: usize, height: usize) -> Result<Self, Box<dyn Error>> {
        let (conn, screen_num) = XCBConnection::connect(None)?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let depth = screen.root_depth;
        let window = conn.generate_id()?;
        let gc = conn.generate_id()?;
        let event_mask = EventMask::EXPOSURE
            | EventMask::KEY_PRESS
            | EventMask::KEY_RELEASE
            | EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::STRUCTURE_NOTIFY;

        conn.create_window(
            0,
            window,
            root,
            0,
            0,
            width as u16,
            height as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(event_mask),
        )?;
        conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(event_mask),
        )?;
        conn.create_gc(gc, window, &CreateGCAux::new())?;

        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"cozyui",
        )?;
        conn.map_window(window)?;
        conn.flush()?;
        let keyboard = text_input::Keyboard::new(&conn)?;
        let shm_image = Self::open_shm_image(&conn, width, height).ok();

        Ok(Self {
            conn,
            window,
            gc,
            depth,
            keyboard,
            upload_buffer: Vec::new(),
            shm_image,
        })
    }

    #[cfg(unix)]
    fn open_shm_image(
        conn: &XCBConnection,
        width: usize,
        height: usize,
    ) -> Result<ShmImage, Box<dyn Error>> {
        let version = conn.shm_query_version()?.reply()?;
        if version.major_version < 1 || (version.major_version == 1 && version.minor_version < 2) {
            return Err("MIT-SHM is too old for fd-backed segments".into());
        }

        let seg = conn.generate_id()?;
        let size = (width * height * Framebuffer::BYTES_PER_PIXEL) as u32;
        let reply = conn.shm_create_segment(seg, size, false)?.reply()?;
        let fd: OwnedFd = reply.shm_fd;
        let file = File::from(fd);
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        Ok(ShmImage { seg, mmap })
    }

    #[cfg(not(unix))]
    fn open_shm_image(
        _conn: &XCBConnection,
        _width: usize,
        _height: usize,
    ) -> Result<ShmImage, Box<dyn Error>> {
        Err("MIT-SHM fd passing requires Unix".into())
    }

    pub(crate) fn draw(&mut self, fb: &Framebuffer) -> Result<(), Box<dyn Error>> {
        if let Some(shm_image) = &mut self.shm_image {
            shm_image.mmap[..fb.ximage_bytes().len()].copy_from_slice(fb.ximage_bytes());
            self.conn.shm_put_image(
                self.window,
                self.gc,
                fb.width as u16,
                fb.height as u16,
                0,
                0,
                fb.width as u16,
                fb.height as u16,
                0,
                0,
                self.depth,
                u8::from(ImageFormat::Z_PIXMAP),
                false,
                shm_image.seg,
                0,
            )?;
            self.conn.flush()?;
            return Ok(());
        }

        self.conn.put_image(
            ImageFormat::Z_PIXMAP,
            self.window,
            self.gc,
            fb.width as u16,
            fb.height as u16,
            0,
            0,
            0,
            self.depth,
            fb.ximage_bytes(),
        )?;
        self.conn.flush()?;
        Ok(())
    }

    pub(crate) fn resize(&mut self, width: usize, height: usize) -> Result<(), Box<dyn Error>> {
        self.conn.configure_window(
            self.window,
            &ConfigureWindowAux::new()
                .width(width as u32)
                .height(height as u32),
        )?;

        if let Some(shm_image) = self.shm_image.take() {
            self.conn.shm_detach(shm_image.seg)?;
        }
        self.shm_image = Self::open_shm_image(&self.conn, width, height).ok();
        self.conn.flush()?;
        Ok(())
    }

    pub(crate) fn draw_rect(&mut self, fb: &Framebuffer, rect: Rect) -> Result<(), Box<dyn Error>> {
        if rect.x == 0 && rect.y == 0 && rect.w == fb.width && rect.h == fb.height {
            return self.draw(fb);
        }

        let byte_len = rect.w * rect.h * Framebuffer::BYTES_PER_PIXEL;
        if let Some(shm_image) = &mut self.shm_image {
            for y in 0..rect.h {
                let dst_start = y * rect.w * Framebuffer::BYTES_PER_PIXEL;
                let dst_end = dst_start + rect.w * Framebuffer::BYTES_PER_PIXEL;
                shm_image.mmap[dst_start..dst_end].copy_from_slice(fb.row_bytes(
                    rect.y + y,
                    rect.x,
                    rect.w,
                ));
            }
            self.conn.shm_put_image(
                self.window,
                self.gc,
                rect.w as u16,
                rect.h as u16,
                0,
                0,
                rect.w as u16,
                rect.h as u16,
                rect.x as i16,
                rect.y as i16,
                self.depth,
                u8::from(ImageFormat::Z_PIXMAP),
                false,
                shm_image.seg,
                0,
            )?;
            self.conn.flush()?;
            return Ok(());
        }

        self.upload_buffer.resize(byte_len, 0);
        for y in 0..rect.h {
            let dst_start = y * rect.w * Framebuffer::BYTES_PER_PIXEL;
            let dst_end = dst_start + rect.w * Framebuffer::BYTES_PER_PIXEL;
            self.upload_buffer[dst_start..dst_end].copy_from_slice(fb.row_bytes(
                rect.y + y,
                rect.x,
                rect.w,
            ));
        }

        self.conn.put_image(
            ImageFormat::Z_PIXMAP,
            self.window,
            self.gc,
            rect.w as u16,
            rect.h as u16,
            rect.x as i16,
            rect.y as i16,
            0,
            self.depth,
            &self.upload_buffer,
        )?;
        self.conn.flush()?;
        Ok(())
    }
}

impl Drop for XWindow {
    fn drop(&mut self) {
        if let Some(shm_image) = &self.shm_image {
            let _ = self.conn.shm_detach(shm_image.seg);
            let _ = self.conn.flush();
        }
    }
}

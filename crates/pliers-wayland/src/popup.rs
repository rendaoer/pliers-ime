//! 候选框的 surface：把一块 `input_popup` role 的 wl_surface 和它的 shm 缓冲区包起来。
//!
//! 关键点：surface 被 `get_input_popup_surface` 指定成 `input_popup` role 之后，
//! **位置就完全由合成器决定了**（niri 会摆在光标下方，放不下就翻到上方），客户端没法
//! 自己挪；能控制的只有"画多大、画什么"。大小 = 你 attach 的 buffer 大小，
//! 而 `attach(None, 0, 0)` 就是隐藏。
//!
//! "画什么" 在 `pliers-popup` crate 里，这里只管怎么贴上去，外加两件事：
//!
//! * **缓冲区轮换**：内容每敲一个字就变，不能一边被合成器显示一边改写，
//!   所以只在收到 `wl_buffer.release`（合成器用完了）之后才重用那块内存
//! * **缩放**：屏幕是 1.5x / 2x 的时候要按倍数多画几倍像素，
//!   再用 `set_buffer_scale` 告诉合成器"这块 buffer 的像素密度是 2 倍"

use std::error::Error;
use std::fs::File;
use std::os::fd::AsFd;

use pliers_engine::Preedit;
use pliers_popup::Painter;
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_v2 as im, zwp_input_popup_surface_v2 as popup,
};

use crate::State;

/// 最多留几块缓冲区。合成器正常都会及时 release，用不到这么多；
/// 留几块是为了"宽度没变、只是字变了"的时候能原地重画
const MAX_SLOTS: usize = 6;

/// 一块 shm 缓冲区：自己的一块内存 + 尺寸 + "合成器还在用吗"
struct Slot {
    buffer: wl_buffer::WlBuffer,
    /// pool 得一起留着：缓冲区只是它里面的一段
    _pool: wl_shm_pool::WlShmPool,
    file: File,
    width: i32,
    height: i32,
    /// true = 已经 attach 出去了，还没收到 release
    busy: bool,
}

pub struct PopupSurface {
    surface: wl_surface::WlSurface,
    /// 留着别 drop：drop 掉候选框就没了
    _popup: popup::ZwpInputPopupSurfaceV2,
    /// 建新缓冲区的时候要用（存着就不用一路传参）
    shm: wl_shm::WlShm,
    qh: QueueHandle<State>,
    slots: Vec<Slot>,
    /// 上一次告诉合成器的缩放
    scale: i32,
}

impl PopupSurface {
    pub fn new(
        compositor: &wl_compositor::WlCompositor,
        shm: &wl_shm::WlShm,
        im_obj: &im::ZwpInputMethodV2,
        qh: &QueueHandle<State>,
    ) -> Result<Self, Box<dyn Error>> {
        let surface = compositor.create_surface(qh, ());
        // 这一步把 surface 定成 input_popup role（一个 surface 只能定一次 role）
        let popup_obj = im_obj.get_input_popup_surface(&surface, qh, ());
        Ok(Self {
            surface,
            _popup: popup_obj,
            shm: shm.clone(),
            qh: qh.clone(),
            slots: Vec::new(),
            scale: 1,
        })
    }

    /// 贴一帧候选框：按 `scale` 画好像素，挑一块能用的缓冲区写进去
    pub fn show(&mut self, painter: &Painter, preedit: &Preedit, scale: i32) {
        self.show_image(painter.render(preedit, scale), scale);
    }

    /// 贴一个中英文模式提示（「中」/「英」）
    pub fn show_notice(&mut self, painter: &Painter, label: &str, scale: i32) {
        self.show_image(painter.render_notice(label, scale), scale);
    }

    /// 贴一帧画好的像素
    fn show_image(&mut self, image: pliers_popup::Image, scale: i32) {
        if image.width <= 0 || image.height <= 0 {
            return; // 没东西可画（正常路径下这会儿该是 hide）
        }

        // 缩放变了得先告诉合成器：它按"这块 buffer 是几倍密度"来算画面大小，
        // 不说的话 2 倍像素会被当成 2 倍大
        if scale != self.scale {
            self.surface.set_buffer_scale(scale);
            self.scale = scale;
        }

        let Some(index) = self.pick(image.width, image.height) else {
            return; // pick 里已经打过日志了
        };
        if let Err(e) = pliers_popup::write(&self.slots[index].file, 0, &image) {
            eprintln!("pliers: 写候选框像素失败：{e}");
            return;
        }
        self.slots[index].busy = true;

        let slot = &self.slots[index];
        self.surface.attach(Some(&slot.buffer), 0, 0);
        // damage 用的是 surface 自己的坐标（逻辑像素），不是 buffer 像素 ——
        // 按 2 倍密度画的时候要除回去
        self.surface
            .damage(0, 0, image.width / scale, image.height / scale);
        self.surface.commit();
    }

    /// attach 一个空 buffer 就是"隐藏"
    pub fn hide(&mut self) {
        self.surface.attach(None, 0, 0);
        self.surface.commit();
    }

    /// 合成器说这块缓冲区用完了，可以重画了
    pub fn release(&mut self, buffer: &wl_buffer::WlBuffer) {
        for slot in &mut self.slots {
            if slot.buffer.id() == buffer.id() {
                slot.busy = false;
            }
        }
    }

    /// 找一块尺寸正好、而且合成器已经还回来的缓冲区；没有就新建一块
    fn pick(&mut self, width: i32, height: i32) -> Option<usize> {
        if let Some(index) = self
            .slots
            .iter()
            .position(|slot| slot.width == width && slot.height == height && !slot.busy)
        {
            return Some(index);
        }
        match self.create_slot(width, height) {
            Ok(index) => Some(index),
            Err(e) => {
                eprintln!("pliers: 建候选框缓冲区失败：{e}");
                None
            }
        }
    }

    /// 新建一块缓冲区：一块自己的 shm + pool + buffer。
    /// 每块内存单独一个文件，比所有缓冲区挤一个 pool 多花几页，但不用管对齐和扩容
    fn create_slot(&mut self, width: i32, height: i32) -> Result<usize, Box<dyn Error>> {
        self.evict(width);
        let size = (width * height * 4) as usize;
        let file = pliers_popup::shm_file(size)?;
        let pool = self
            .shm
            .create_pool(file.as_fd(), size as i32, &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            width,
            height,
            width * 4,
            wl_shm::Format::Argb8888,
            &self.qh,
            (),
        );
        self.slots.push(Slot {
            buffer,
            _pool: pool,
            file,
            width,
            height,
            busy: false,
        });
        Ok(self.slots.len() - 1)
    }

    /// 缓冲区别攒太多：宽度不是现在要用的、而且合成器已经还回来的，直接扔
    fn evict(&mut self, keep_width: i32) {
        self.slots
            .retain(|slot| slot.busy || slot.width == keep_width);
        while self.slots.len() >= MAX_SLOTS {
            match self.slots.iter().position(|slot| !slot.busy) {
                Some(index) => {
                    self.slots.remove(index);
                }
                // 全都在用：合成器一块都没还（正常不会），只能多建一块
                None => break,
            }
        }
    }
}

impl Dispatch<popup::ZwpInputPopupSurfaceV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &popup::ZwpInputPopupSurfaceV2,
        event: popup::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // 合成器告诉我们光标在候选框坐标系里的位置：只是个提示（位置还是它定），
        // 变化时打一行日志，方便确认这条链路是活的
        if let popup::Event::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event
            && state.caret != (x, y, width, height)
        {
            state.caret = (x, y, width, height);
            if state.debug {
                eprintln!("pliers: 光标矩形 {width}x{height} @ ({x},{y})（相对候选框左上角）");
            }
        }
    }
}

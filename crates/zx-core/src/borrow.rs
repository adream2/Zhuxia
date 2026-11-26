//! 单例状态的「互斥借用」守卫。
//!
//! 界面状态（剪贴板面板、任务栏详情页、录屏缓存）都挂在 thread_local 的裸指针上，
//! 访问器以前直接返回 `&'static mut T`。这在**同一个函数里嵌套取两次**时就是
//! 未定义行为——两个 `&mut` 同时存活。类型系统拦不住（`'static` 让它看起来
//! 永远合法），只能靠约定；约定迟早会破。
//!
//! 这里把约定变成可检查的东西：返回值是一个守卫，构造时给线程本地的计数器加一、
//! `Drop` 时减一。**同时存活两个就会在 debug 构建里炸掉**，而不是变成偶发崩溃。
//!
//! 为什么计数能分清「嵌套」和「先后」：守卫的生命周期就是借用的生命周期。
//! `if let Some(p) = get(h) { ... }` 结束时守卫就 Drop 了，所以先后两次取是
//! 1 → 0 → 1，不会累积；只有真正嵌套才会变成 2。
//!
//! 注意：守卫是 `!Send` 的（借的东西是线程局部的），别想着把它传给别的线程。

use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::thread::LocalKey;

/// 同一份状态的借用守卫。用 `Deref` 让调用点看起来还像在用一个 `&mut T`。
pub struct Ref<T> {
    ptr: *mut T,
    depth: &'static LocalKey<Cell<u32>>,
    what: &'static str,
    /// 守卫不能跨线程：借的是 thread_local 里的东西。
    _not_send: PhantomData<*mut ()>,
}

impl<T> Ref<T> {
    /// # Safety
    ///
    /// `ptr` 必须指向一个活的 `T`，并且在守卫存活期间没有别的路径会移动/销毁它。
    /// 这是访问器自己的责任——它从 thread_local 里取指针，只有窗口销毁时才会释放。
    pub unsafe fn new(
        ptr: *mut T,
        depth: &'static LocalKey<Cell<u32>>,
        what: &'static str,
    ) -> Ref<T> {
        let n = depth.with(|c| {
            let n = c.get() + 1;
            c.set(n);
            n
        });
        debug_assert!(
            n == 1,
            "{what} 被同时借用了 {n} 次：同一个函数里嵌套取状态会产生两个 &mut，是 UB"
        );
        Ref { ptr, depth, what, _not_send: PhantomData }
    }

    /// 借用的名字（`debug_assert` 里用，也方便日志）。
    pub fn what(&self) -> &'static str {
        self.what
    }
}

impl<T> Deref for Ref<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.ptr }
    }
}

impl<T> DerefMut for Ref<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.ptr }
    }
}

impl<T> Drop for Ref<T> {
    fn drop(&mut self) {
        self.depth.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static D: Cell<u32> = const { Cell::new(0) };
    }

    fn depth() -> u32 {
        D.with(|c| c.get())
    }

    #[test]
    fn sequential_borrows_do_not_accumulate() {
        let mut v = 7i32;
        for _ in 0..3 {
            {
                let mut g = unsafe { Ref::new(&mut v as *mut i32, &D, "测试值") };
                *g += 1;
                assert_eq!(depth(), 1, "只该有一个借用");
            }
            assert_eq!(depth(), 0, "守卫 Drop 之后额度要还回去");
        }
        assert_eq!(v, 10);
    }

    #[test]
    fn deref_works_like_a_reference() {
        let mut v = 41i32;
        let mut g = unsafe { Ref::new(&mut v as *mut i32, &D, "测试值") };
        *g += 1;
        assert_eq!(*g, 42);
        assert_eq!(g.what(), "测试值");
        drop(g);
        assert_eq!(v, 42);
    }

    // 嵌套是 UB，debug 下一律炸掉——这条测试就是那个断言的回归。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "被同时借用了 2 次")]
    fn nested_borrow_is_caught_in_debug() {
        let mut v = 0i32;
        let _outer = unsafe { Ref::new(&mut v as *mut i32, &D, "测试值") };
        let _inner = unsafe { Ref::new(&mut v as *mut i32, &D, "测试值") };
    }
}

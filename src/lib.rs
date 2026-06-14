use ibverbs_sys::*;
use nix::sys::socket::{recv, send, MsgFlags};
use std::mem::zeroed;

/// RDMA 控制通道 TCP 端口
pub const CONTROL_PORT: u16 = 9999;

/// RDMA 连接信息，通过 TCP 控制通道交换
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ConnInfo {
    pub qp_num: u32,
    pub rkey: u32,
    pub buf_addr: u64,
    pub lid: u16,
    pub port: u8,
}

impl ConnInfo {
    /// 转为网络字节序（大端），用于跨架构兼容
    pub fn to_be(&self) -> Self {
        Self {
            qp_num: self.qp_num.to_be(),
            rkey: self.rkey.to_be(),
            buf_addr: self.buf_addr.to_be(),
            lid: self.lid.to_be(),
            port: self.port,
        }
    }

    /// 从网络字节序转回主机字节序
    pub fn from_be(&self) -> Self {
        Self {
            qp_num: u32::from_be(self.qp_num),
            rkey: u32::from_be(self.rkey),
            buf_addr: u64::from_be(self.buf_addr),
            lid: u16::from_be(self.lid),
            port: self.port,
        }
    }

    /// 序列化为字节切片
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self as *const _ as *const u8, size_of_val(self)) }
    }

    /// 从字节切片反序列化（原地覆盖）
    pub unsafe fn from_bytes(&mut self, buf: &[u8]) {
        std::ptr::copy_nonoverlapping(buf.as_ptr(), self as *mut _ as *mut u8, size_of_val(self));
    }
}

/// 确保 TCP fd 上完整发送所有字节（处理部分发送）
pub fn tcp_send_all(fd: i32, buf: &[u8]) -> Result<(), String> {
    let mut pos = 0;
    while pos < buf.len() {
        let n =
            send(fd, &buf[pos..], MsgFlags::empty()).map_err(|e| format!("send 失败: {:?}", e))?;
        if n == 0 {
            return Err("TCP 连接断开 (send=0)".into());
        }
        pos += n;
    }
    Ok(())
}

/// 确保 TCP fd 上完整接收指定字节数（处理部分接收）
pub fn tcp_recv_exact(fd: i32, buf: &mut [u8]) -> Result<(), String> {
    let mut pos = 0;
    while pos < buf.len() {
        let n = recv(fd, &mut buf[pos..], MsgFlags::empty())
            .map_err(|e| format!("recv 失败: {:?}", e))?;
        if n == 0 {
            return Err("TCP 连接断开 (recv=0)".into());
        }
        pos += n;
    }
    Ok(())
}

/// 查询端口的 LID (Local Identifier)
///
/// # Safety
/// `ctx` 必须是有效的 ibv_context 指针
pub unsafe fn get_lid(ctx: *mut ibv_context, port: u8) -> u16 {
    let mut port_attr: ibv_port_attr = zeroed();
    let ret = ibv_query_port(ctx, port, &mut port_attr as *mut _);
    assert_eq!(ret, 0, "ibv_query_port 失败, ret={}", ret);
    port_attr.lid
}

/// 查询端口的 active MTU
///
/// # Safety
/// `ctx` 必须是有效的 ibv_context 指针
pub unsafe fn get_port_mtu(ctx: *mut ibv_context, port: u8) -> ibv_mtu {
    let mut port_attr: ibv_port_attr = zeroed();
    let ret = ibv_query_port(ctx, port, &mut port_attr as *mut _);
    assert_eq!(ret, 0, "ibv_query_port MTU 失败, ret={}", ret);
    port_attr.active_mtu
}

use ibverbs_sys::*;
use nix::sys::socket::{recv, send, MsgFlags};
use std::mem::zeroed;

/// 断言宏：条件为假时打印错误消息并退出
///
/// # Usage
/// ```ignore
/// check!(ptr != null(), "指针为空");
/// check!(ret == 0, &format!("操作失败, ret={}", ret));
/// ```
#[macro_export]
macro_rules! check {
    ($cond:expr, $msg:expr) => {{
        if !$cond {
            eprintln!("{}", $msg);
            std::process::exit(1);
        }
    }};
}

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

/// QP 状态转换：RESET → INIT
///
/// # Safety
/// `ctx` 必须是有效的 ibv_context 指针，`qp` 必须是有效的 ibv_qp 指针
pub unsafe fn qp_to_init(qp: *mut ibv_qp, port: u8, access_flags: u32) -> Result<(), String> {
    let mut attr: ibv_qp_attr = zeroed();
    attr.qp_state = ibv_qp_state::IBV_QPS_INIT;
    attr.pkey_index = 0;
    attr.port_num = port;
    attr.qp_access_flags = access_flags;
    let mask = (IBV_QP_STATE.0
        | IBV_QP_PKEY_INDEX.0
        | IBV_QP_PORT.0
        | IBV_QP_ACCESS_FLAGS.0) as i32;
    let ret = ibv_modify_qp(qp, &mut attr, mask);
    if ret != 0 {
        return Err(format!("QP → INIT 失败, ret={}", ret));
    }
    Ok(())
}

/// QP 状态转换：INIT → RTR
///
/// # Safety
/// `qp` 必须是有效的 ibv_qp 指针
pub unsafe fn qp_to_rtr(
    qp: *mut ibv_qp,
    dest_qpn: u32,
    dest_lid: u16,
    path_mtu: ibv_mtu,
    port: u8,
) -> Result<(), String> {
    let mut attr: ibv_qp_attr = zeroed();
    attr.qp_state = ibv_qp_state::IBV_QPS_RTR;
    attr.dest_qp_num = dest_qpn;
    attr.rq_psn = 0;
    attr.max_dest_rd_atomic = 1;
    attr.min_rnr_timer = 12;
    attr.path_mtu = path_mtu;
    attr.ah_attr.dlid = dest_lid;
    attr.ah_attr.sl = 0;
    attr.ah_attr.src_path_bits = 0;
    attr.ah_attr.static_rate = 0;
    attr.ah_attr.is_global = 0;
    attr.ah_attr.port_num = port;
    let mask = (IBV_QP_STATE.0
        | IBV_QP_AV.0
        | IBV_QP_PATH_MTU.0
        | IBV_QP_DEST_QPN.0
        | IBV_QP_RQ_PSN.0
        | IBV_QP_MAX_DEST_RD_ATOMIC.0
        | IBV_QP_MIN_RNR_TIMER.0) as i32;
    let ret = ibv_modify_qp(qp, &mut attr, mask);
    if ret != 0 {
        return Err(format!("QP → RTR 失败, ret={}", ret));
    }
    Ok(())
}

/// QP 状态转换：RTR → RTS
///
/// # Safety
/// `qp` 必须是有效的 ibv_qp 指针
pub unsafe fn qp_to_rts(qp: *mut ibv_qp) -> Result<(), String> {
    let mut attr: ibv_qp_attr = zeroed();
    attr.qp_state = ibv_qp_state::IBV_QPS_RTS;
    attr.sq_psn = 0;
    attr.timeout = 14;
    attr.retry_cnt = 7;
    attr.rnr_retry = 7;
    attr.max_rd_atomic = 1;
    let mask = (IBV_QP_STATE.0
        | IBV_QP_SQ_PSN.0
        | IBV_QP_TIMEOUT.0
        | IBV_QP_RETRY_CNT.0
        | IBV_QP_RNR_RETRY.0
        | IBV_QP_MAX_QP_RD_ATOMIC.0) as i32;
    let ret = ibv_modify_qp(qp, &mut attr, mask);
    if ret != 0 {
        return Err(format!("QP → RTS 失败, ret={}", ret));
    }
    Ok(())
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

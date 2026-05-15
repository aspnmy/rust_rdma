use ibverbs_sys::*;
use nix::sys::socket::*;
use std::alloc::{alloc, Layout};
use std::mem::zeroed;
use std::os::raw::c_void;
use std::ptr;

const MEM_SIZE: usize = 4096;
const CONTROL_PORT: u16 = 9999;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ConnInfo {
    pub qp_num: u32,
    pub rkey: u32,
    pub buf_addr: u64,
}

fn main() {
    unsafe {
        // --------------------------
        // 分配 RDMA 共享内存
        // --------------------------
        let layout = Layout::from_size_align(MEM_SIZE, 4096).unwrap();
        let buf = alloc(layout);
        ptr::write_bytes(buf, 0xAA, MEM_SIZE);
        println!("[服务端] 分配共享内存: {:p}", buf);

        // --------------------------
        // 初始化 RDMA 设备
        // --------------------------
        let dev_list = ibv_get_device_list(ptr::null_mut());
        let dev = *dev_list;
        let ctx = ibv_open_device(dev);
        let pd = ibv_alloc_pd(ctx);

        // 注册内存（允许远程读写）
        let mr = ibv_reg_mr(
            pd,
            buf as *mut c_void,
            MEM_SIZE,
            (ibverbs_sys::IBV_ACCESS_LOCAL_WRITE.0
                | ibverbs_sys::IBV_ACCESS_REMOTE_WRITE.0
                | ibverbs_sys::IBV_ACCESS_REMOTE_READ.0) as i32,
        );

        // --------------------------
        // 创建 CQ / QP
        // --------------------------
        let cq = ibv_create_cq(ctx, 128, ptr::null_mut(), ptr::null_mut(), 0);

        let mut qp_init: ibv_qp_init_attr = zeroed();
        qp_init.send_cq = cq;
        qp_init.recv_cq = cq;
        qp_init.qp_type = ibv_qp_type::IBV_QPT_RC;
        qp_init.sq_sig_all = 1;
        qp_init.cap.max_send_wr = 32;
        qp_init.cap.max_recv_wr = 32;
        qp_init.cap.max_send_sge = 1;
        qp_init.cap.max_recv_sge = 1;

        let qp = ibv_create_qp(pd, &mut qp_init);

        // --------------------------
        // QP 状态转换
        // --------------------------
        let mut attr: ibv_qp_attr = zeroed();
        attr.qp_state = ibv_qp_state::IBV_QPS_INIT;
        attr.pkey_index = 0;
        attr.port_num = 1;
        ibv_modify_qp(
            qp,
            &mut attr,
            (ibverbs_sys::IBV_QP_STATE.0
                | ibverbs_sys::IBV_QP_PKEY_INDEX.0
                | ibverbs_sys::IBV_QP_PORT.0) as i32,
        );

        attr.qp_state = ibv_qp_state::IBV_QPS_RTR;
        ibv_modify_qp(qp, &mut attr, ibverbs_sys::IBV_QP_STATE.0 as i32);

        attr.qp_state = ibv_qp_state::IBV_QPS_RTS;
        ibv_modify_qp(qp, &mut attr, ibverbs_sys::IBV_QP_STATE.0 as i32);

        // --------------------------
        // 控制通道：监听客户端
        // --------------------------
        let sock = socket(AddressFamily::Inet, SockType::Stream, SockFlag::empty(), None).unwrap();
        let sa = SockaddrIn::new(0, 0, 0, 0, CONTROL_PORT);
        bind(sock.as_raw_fd(), &sa).unwrap();
        listen(&sock, 1).unwrap();
        println!("[服务端] 等待客户端连接...");

        let client_fd = accept(sock.as_raw_fd()).unwrap();

        let info = ConnInfo {
            qp_num: (*qp).qp_num,
            rkey: (*mr).rkey,
            buf_addr: buf as u64,
        };

        let buf = unsafe {
            std::slice::from_raw_parts(&info as *const _ as *const u8, std::mem::size_of::<ConnInfo>())
        };
        send(client_fd, buf, MsgFlags::empty()).unwrap();
        println!("[服务端] 发送连接信息成功");
        println!("[服务端] RKey: 0x{:x}", (*mr).rkey);
        println!("[服务端] 运行中... 客户端可直接读写内存");

        loop {
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
    }
}
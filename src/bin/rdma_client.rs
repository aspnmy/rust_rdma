use ibverbs_sys::*;
use nix::sys::socket::*;
use nix::unistd::close;
use std::mem::zeroed;
use std::net::Ipv4Addr;
use std::os::raw::c_void;
use std::ptr;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ConnInfo {
    pub qp_num: u32,
    pub rkey: u32,
    pub buf_addr: u64,
}

const CONTROL_PORT: u16 = 9999;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: {} <服务端IP>", args[0]);
        return;
    }

    let server_ip = &args[1];

    unsafe {
        // --------------------------
        // 连接服务端获取信息
        // --------------------------
        let sock = socket(AddressFamily::Inet, SockType::Stream, SockFlag::empty(), None).unwrap();
        let ip = server_ip.parse::<Ipv4Addr>().unwrap();
        let octets = ip.octets();
        let sa = SockaddrIn::new(octets[0], octets[1], octets[2], octets[3], CONTROL_PORT);
        connect(sock, &sa).unwrap();

        let mut info: ConnInfo = zeroed();
        recv(sock, &mut info as *mut _ as *mut u8, std::mem::size_of::<ConnInfo>(), MsgFlags::empty()).unwrap();
        close(sock);

        println!("[客户端] 成功获取服务端信息");
        println!("  QP: {}", info.qp_num);
        println!("  RKey: 0x{:x}", info.rkey);
        println!("  远程内存地址: 0x{:x}", info.buf_addr);

        // --------------------------
        // 初始化本地 RDMA
        // --------------------------
        let dev_list = ibv_get_device_list(ptr::null_mut());
        let ctx = ibv_open_device(*dev_list);
        let pd = ibv_alloc_pd(ctx);
        let cq = ibv_create_cq(ctx, 128, ptr::null_mut(), ptr::null_mut(), 0);

        let mut buf = vec![0u8; 4096];
        let mr = ibv_reg_mr(
            pd,
            buf.as_mut_ptr() as *mut c_void,
            buf.len(),
            ibverbs_sys::IBV_ACCESS_LOCAL_WRITE.0 as i32,
        );

        // --------------------------
        // 创建 QP
        // --------------------------
        let mut qp_init: ibv_qp_init_attr = zeroed();
        qp_init.send_cq = cq;
        qp_init.recv_cq = cq;
        qp_init.qp_type = ibv_qp_type::IBV_QPT_RC;
        qp_init.cap.max_send_wr = 32;
        qp_init.cap.max_recv_wr = 32;
        qp_init.cap.max_send_sge = 1;
        qp_init.cap.max_recv_sge = 1;

        let qp = ibv_create_qp(pd, &qp_init);

        let mut attr: ibv_qp_attr = zeroed();
        attr.qp_state = ibv_qp_state::IBV_QPS_INIT;
        attr.port_num = 1;
        ibv_modify_qp(
            qp,
            &mut attr,
            (ibverbs_sys::IBV_QP_STATE.0 | ibverbs_sys::IBV_QP_PORT.0) as i32,
        );

        // --------------------------
        // 连接到服务端 QP
        // --------------------------
        attr.qp_state = ibv_qp_state::IBV_QPS_RTR;
        attr.dest_qp_num = info.qp_num;
        attr.rq_psn = 0;
        ibv_modify_qp(
            qp,
            &mut attr,
            (ibverbs_sys::IBV_QP_STATE.0
                | ibverbs_sys::IBV_QP_DEST_QPN.0
                | ibverbs_sys::IBV_QP_RQ_PSN.0) as i32,
        );

        attr.qp_state = ibv_qp_state::IBV_QPS_RTS;
        attr.sq_psn = 0;
        ibv_modify_qp(
            qp,
            &mut attr,
            (ibverbs_sys::IBV_QP_STATE.0 | ibverbs_sys::IBV_QP_SQ_PSN.0) as i32,
        );

        println!("[客户端] RDMA 连接建立完成！");
        println!("========================================");
        println!("现在你可以：");
        println!("  1. RDMA Write  写服务端内存");
        println!("  2. RDMA Read   读服务端内存");
        println!("========================================");

        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}
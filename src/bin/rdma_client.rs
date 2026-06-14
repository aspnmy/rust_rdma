use ibverbs_sys::*;
use ibverbs_sys::{ibv_access_flags, ibv_send_flags};
use nix::sys::socket::*;
use rust_rdma::{
    check, get_lid, get_port_mtu, poll_cq, post_send, qp_to_init, qp_to_rtr, qp_to_rts,
    tcp_recv_exact, tcp_send_all, ConnInfo, CONTROL_PORT,
};
use std::mem::{size_of, zeroed};
use std::os::fd::AsRawFd;
use std::os::raw::c_void;
use std::ptr;

const DATA_SIZE: usize = 64;
const LOCAL_MEM_SIZE: usize = 4096;

/// 客户端专用，带 "[客户端]" 前缀的断言
#[inline]
fn cli_check(cond: bool, msg: &str) {
    check!(cond, &format!("[客户端] 错误: {}", msg));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: {} <服务端IP>", args[0]);
        return;
    }
    let server_ip = &args[1];

    unsafe {
        println!("[客户端] === RDMA Client 启动 ===");
        println!("[客户端] 目标服务端: {}", server_ip);

        // ============== 步进 1: TCP 控制通道 ==============
        println!("[客户端] 步进 1/8: TCP 连接服务端");
        let sock = socket(
            AddressFamily::Inet,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )
        .unwrap();
        let ip = server_ip.parse::<std::net::Ipv4Addr>().unwrap();
        let octets = ip.octets();
        let sa = SockaddrIn::new(octets[0], octets[1], octets[2], octets[3], CONTROL_PORT);
        connect(sock.as_raw_fd(), &sa).unwrap();
        println!("[客户端]    已连接服务端 {}:{}", server_ip, CONTROL_PORT);

        // 接收服务端 ConnInfo (字节序还原)
        let mut raw_buf = vec![0u8; size_of::<ConnInfo>()];
        tcp_recv_exact(sock.as_raw_fd(), &mut raw_buf).unwrap();
        let mut server_info: ConnInfo = zeroed();
        server_info.from_bytes(&raw_buf);
        let server_info = server_info.from_be();
        println!(
            "[客户端]    收到服务端 ConnInfo (QP={}, LID={}, RKey=0x{:x}, Buf=0x{:x})",
            server_info.qp_num, server_info.lid, server_info.rkey, server_info.buf_addr
        );

        // ============== 步进 2: 初始化本地 RDMA ==============
        println!("[客户端] 步进 2/8: 打开 RDMA 设备");
        let dev_list = ibv_get_device_list(ptr::null_mut());
        cli_check(!dev_list.is_null(), "ibv_get_device_list 失败");
        let dev = *dev_list;
        cli_check(!dev.is_null(), "没有可用的 RDMA 设备");
        let ctx = ibv_open_device(dev);
        cli_check(!ctx.is_null(), "ibv_open_device 失败");
        ibv_free_device_list(dev_list);

        println!("[客户端] 步进 3/8: 分配 PD + 注册本地 MR");
        let pd = ibv_alloc_pd(ctx);
        cli_check(!pd.is_null(), "ibv_alloc_pd 失败");

        // 分配本地内存作为 RDMA Write 的数据源
        let mut local_buf = vec![0u8; LOCAL_MEM_SIZE];
        let msg = b"Hello from RDMA Client! This data was written via RDMA Write.";
        let write_len = DATA_SIZE.min(local_buf.len());
        let copy_len = msg.len().min(write_len);
        local_buf[..copy_len].copy_from_slice(&msg[..copy_len]);
        if copy_len < write_len {
            let footer = b"\n[EOF]";
            let footer_len = footer.len().min(write_len - copy_len);
            local_buf[copy_len..copy_len + footer_len].copy_from_slice(&footer[..footer_len]);
        }

        let mr = ibv_reg_mr(
            pd,
            local_buf.as_mut_ptr() as *mut c_void,
            local_buf.len(),
            ibv_access_flags::IBV_ACCESS_LOCAL_WRITE.0 as i32,
        );
        cli_check(!mr.is_null(), "ibv_reg_mr 失败");

        println!("[客户端] 步进 4/8: 创建 CQ + QP");
        let cq = ibv_create_cq(ctx, 128, ptr::null_mut(), ptr::null_mut(), 0);
        cli_check(!cq.is_null(), "ibv_create_cq 失败");

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
        cli_check(!qp.is_null(), "ibv_create_qp 失败");

        // ============== 步进 5: QP → INIT + 查询 LID ==============
        println!("[客户端] 步进 5/8: QP → INIT");
        qp_to_init(qp, 1, 0).unwrap_or_else(|e| panic!("[客户端] {}", e));

        let client_lid = get_lid(ctx, 1);
        let active_mtu = get_port_mtu(ctx, 1);
        println!(
            "[客户端]    本端 LID: {}, active MTU: {:?}",
            client_lid, active_mtu
        );

        // 发送客户端 ConnInfo → 服务端 (字节序转换)
        let client_info = ConnInfo {
            qp_num: (*qp).qp_num,
            rkey: (*mr).rkey,
            buf_addr: local_buf.as_ptr() as u64,
            lid: client_lid,
            port: 1,
        };
        let be_info = client_info.to_be();
        tcp_send_all(sock.as_raw_fd(), be_info.as_bytes()).unwrap();
        println!(
            "[客户端]    已发送客户端 ConnInfo (QP={}, LID={})",
            client_info.qp_num, client_info.lid
        );
        drop(sock);

        // ============== 步进 6-7: QP → RTR → RTS ==============
        println!("[客户端] 步进 6/8: QP → RTR");
        qp_to_rtr(qp, server_info.qp_num, server_info.lid, active_mtu, 1)
            .unwrap_or_else(|e| panic!("[客户端] {}", e));

        println!("[客户端] 步进 7/8: QP → RTS");
        qp_to_rts(qp).unwrap_or_else(|e| panic!("[客户端] {}", e));

        // ============== 步进 8: 执行 RDMA Write ==============
        println!("[客户端] 步进 8/8: 执行 RDMA Write...");

        // 构造 SGE (Scatter/Gather Element)
        let mut sge = ibv_sge {
            addr: local_buf.as_ptr() as u64,
            length: DATA_SIZE as u32,
            lkey: (*mr).lkey,
        };

        // 构造 Send WR
        let mut wr: ibv_send_wr = zeroed();
        wr.wr_id = 1;
        wr.next = ptr::null_mut();
        wr.sg_list = &mut sge;
        wr.num_sge = 1;
        wr.opcode = ibv_wr_opcode::IBV_WR_RDMA_WRITE;
        wr.send_flags = ibv_send_flags::IBV_SEND_SIGNALED.0;
        wr.wr.rdma.remote_addr = server_info.buf_addr;
        wr.wr.rdma.rkey = server_info.rkey;

        let mut bad_wr: *mut ibv_send_wr = ptr::null_mut();
        let ret = post_send(qp, &mut wr, &mut bad_wr);
        cli_check(ret == 0, &format!("ibv_post_send 失败, ret={}", ret));
        println!("[客户端]    RDMA Write 请求已提交");

        // 轮询 CQ 获取完成事件
        let mut wc: ibv_wc = zeroed();
        let mut poll_count = 0;
        loop {
            let ne = poll_cq(cq, 1, &mut wc);
            if ne > 0 {
                break;
            }
            if ne < 0 {
                panic!("[客户端] ibv_poll_cq 错误: {}", ne);
            }
            poll_count += 1;
            if poll_count > 5000 {
                panic!("[客户端] 轮询超时 (5s)");
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        cli_check(
            wc.is_valid(),
            &format!("RDMA Write 完成状态异常: {:?}", wc.error().map(|(s, _)| s)),
        );
        println!("[客户端]    ✓ RDMA Write 完成! (WC status=SUCCESS)");
        println!(
            "[客户端]    写到服务端 0x{:x} (rkey=0x{:x}) 共 {} 字节",
            server_info.buf_addr,
            server_info.rkey,
            wc.len(),
        );
        println!("[客户端]    写入数据: {:?}", &local_buf[..DATA_SIZE]);

        // RDMA 资源清理
        ibv_destroy_qp(qp);
        ibv_destroy_cq(cq);
        ibv_dereg_mr(mr);
        ibv_dealloc_pd(pd);
        ibv_close_device(ctx);
        println!("[客户端] === RDMA Write 成功! ===");
    }
}

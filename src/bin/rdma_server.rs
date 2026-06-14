use ibverbs_sys::ibv_access_flags;
use ibverbs_sys::*;
use nix::sys::socket::*;
use nix::unistd::close;
use rust_rdma::{
    check, get_lid, get_port_mtu, qp_to_init, qp_to_rtr, qp_to_rts, tcp_recv_exact, tcp_send_all,
    ConnInfo, CONTROL_PORT,
};
use std::alloc::{alloc, dealloc, Layout};
use std::mem::{size_of, zeroed};
use std::os::fd::AsRawFd;
use std::os::raw::c_void;
use std::ptr;

const MEM_SIZE: usize = 4096;

/// 服务端专用，带 "[服务端]" 前缀的断言
#[inline]
fn srv_check(cond: bool, msg: &str) {
    check!(cond, &format!("[服务端] 错误: {}", msg));
}

fn main() {
    unsafe {
        println!("[服务端] === RDMA Server 启动 ===");
        println!("[服务端] 步进 1/8: 打开 RDMA 设备");

        let dev_list = ibv_get_device_list(ptr::null_mut());
        srv_check(
            !dev_list.is_null(),
            "ibv_get_device_list 失败 (无 RDMA 设备?)",
        );
        let dev = *dev_list;
        srv_check(!dev.is_null(), "没有可用的 RDMA 设备");
        let ctx = ibv_open_device(dev);
        srv_check(!ctx.is_null(), "ibv_open_device 失败");
        ibv_free_device_list(dev_list);

        println!("[服务端] 步进 2/8: 分配保护域 (PD)");
        let pd = ibv_alloc_pd(ctx);
        srv_check(!pd.is_null(), "ibv_alloc_pd 失败");

        println!("[服务端] 步进 3/8: 分配共享内存 + 注册 MR");
        let layout = Layout::from_size_align(MEM_SIZE, 4096).unwrap();
        let buf = alloc(layout);
        srv_check(!buf.is_null(), "alloc 失败");
        ptr::write_bytes(buf, 0xAA, MEM_SIZE);
        println!("[服务端]    共享内存: {:p} (已填充 0xAA)", buf);

        let mr = ibv_reg_mr(
            pd,
            buf as *mut c_void,
            MEM_SIZE,
            (ibv_access_flags::IBV_ACCESS_LOCAL_WRITE
                | ibv_access_flags::IBV_ACCESS_REMOTE_WRITE
                | ibv_access_flags::IBV_ACCESS_REMOTE_READ)
                .0 as i32,
        );
        srv_check(!mr.is_null(), "ibv_reg_mr 失败");

        println!("[服务端] 步进 4/8: 创建 CQ + QP");
        let cq = ibv_create_cq(ctx, 128, ptr::null_mut(), ptr::null_mut(), 0);
        srv_check(!cq.is_null(), "ibv_create_cq 失败");

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
        srv_check(!qp.is_null(), "ibv_create_qp 失败");

        // ============== QP → INIT ==============
        println!("[服务端] 步进 5/8: QP 状态转换 INIT");
        qp_to_init(
            qp,
            1,
            (ibv_access_flags::IBV_ACCESS_REMOTE_WRITE | ibv_access_flags::IBV_ACCESS_REMOTE_READ)
                .0,
        )
        .unwrap_or_else(|e| panic!("[服务端] {}", e));

        let lid = get_lid(ctx, 1);
        let active_mtu = get_port_mtu(ctx, 1);
        println!(
            "[服务端]    本端 LID: {}, active MTU: {:?}",
            lid, active_mtu
        );

        // ============== TCP 控制通道 ==============
        println!("[服务端] 步进 6/8: TCP 控制通道等待客户端...");
        let sock = socket(
            AddressFamily::Inet,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )
        .unwrap();
        let sa = SockaddrIn::new(0, 0, 0, 0, CONTROL_PORT);
        bind(sock.as_raw_fd(), &sa).unwrap();
        listen(&sock, Backlog::new(5).unwrap()).unwrap();

        let client_fd = accept(sock.as_raw_fd()).unwrap();
        println!("[服务端]    客户端已连接 (fd={})", client_fd);

        // 发送服务端 ConnInfo → 客户端 (字节序转换)
        let info = ConnInfo {
            qp_num: (*qp).qp_num,
            rkey: (*mr).rkey,
            buf_addr: buf as u64,
            lid,
            port: 1,
        };
        let be_info = info.to_be();
        tcp_send_all(client_fd, be_info.as_bytes()).unwrap();
        println!(
            "[服务端]    已发送 ConnInfo (QP={}, RKey=0x{:x})",
            info.qp_num, info.rkey
        );

        // 接收客户端 ConnInfo (字节序还原)
        let mut client_info: ConnInfo = zeroed();
        let mut raw_buf = vec![0u8; size_of::<ConnInfo>()];
        tcp_recv_exact(client_fd, &mut raw_buf).unwrap();
        client_info.from_bytes(&raw_buf);
        let client_info = client_info.from_be();
        println!(
            "[服务端]    收到客户端 ConnInfo (QP={}, LID={})",
            client_info.qp_num, client_info.lid
        );
        close(client_fd).unwrap_or_else(|e| eprintln!("[服务端] 关闭 client fd 警告: {:?}", e));
        drop(sock);

        // ============== QP → RTR → RTS ==============
        println!("[服务端] 步进 7/8: QP → RTR → RTS");
        qp_to_rtr(qp, client_info.qp_num, client_info.lid, active_mtu, 1)
            .unwrap_or_else(|e| panic!("[服务端] {}", e));
        qp_to_rts(qp).unwrap_or_else(|e| panic!("[服务端] {}", e));

        println!("[服务端] 步进 8/8: RDMA 连接已就绪 ✓");
        println!("[服务端]    等待客户端 RDMA Write...");
        println!(
            "[服务端]    当前共享内存首字节: 0x{:02x}",
            *(buf as *const u8)
        );

        // 轮询检测内存变更
        for i in 0..60 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let first = *(buf as *const u8);
            if first != 0xAA {
                let buf_slice = std::slice::from_raw_parts(buf as *const u8, 64);
                println!(
                    "[服务端] !!! 第 {} 秒检测到内存变更: 0x{:02x} → 0x{:02x} !!!",
                    i + 1,
                    0xAA,
                    first
                );
                println!("[服务端]    共享内存前 64 字节:");
                for (j, &byte) in buf_slice.iter().enumerate() {
                    if j % 16 == 0 {
                        print!("[服务端]    {:04x}: ", j);
                    }
                    print!("{:02x} ", byte);
                    if j % 16 == 15 {
                        println!();
                    }
                }
                println!();
                println!("[服务端] ✓ RDMA Write 验证成功!");
                break;
            }
        }

        if *(buf as *const u8) == 0xAA {
            println!("[服务端] 未检测到内存变更 (超时 60s)");
        }

        // 清理 RDMA 资源
        ibv_destroy_qp(qp);
        ibv_destroy_cq(cq);
        ibv_dereg_mr(mr);
        dealloc(buf, layout);
        ibv_dealloc_pd(pd);
        ibv_close_device(ctx);
        println!("[服务端] === 运行结束 ===");
    }
}

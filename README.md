# rust_rdma

Rust RDMA (InfiniBand/RoCE) 演示项目，展示通过 RDMA Write 在两台机器间进行远程内存直接访问的完整流程。

## 架构

```
┌─────────────────┐          TCP 控制通道 (9999)         ┌─────────────────┐
│  RDMA Server    │ ◄──── 交换 ConnInfo (QP/LID/rkey) ──► │  RDMA Client    │
│                 │                                       │                 │
│  共享内存 4096B │ ◄────── RDMA Write (64B) ──────────── │  数据源 buffer  │
│  (初始填充 0xAA) │                                       │  (Hello msg)    │
└─────────────────┘                                       └─────────────────┘
```

## 前提条件

- Linux 系统（或 WSL2）with RDMA 硬件（InfiniBand 或 RoCE）
- 安装 `rdma-core` 和 `libibverbs-dev`
- Rust 工具链 1.82+

```bash
# Ubuntu/Debian
sudo apt install rdma-core libibverbs-dev librdmacm-dev

# 查看 RDMA 设备
ibv_devinfo
```

## 运行

**服务端（被动等待 RDMA Write）：**
```bash
cargo run --release --bin rdma_server
```

**客户端（发起 RDMA Write）：**
```bash
cargo run --release --bin rdma_client <服务端IP>
```

服务端启动后在 TCP 9999 端口等待客户端。客户端连接后交换 RDMA 连接信息（QP号、LID、rkey、内存地址），然后执行 RDMA Write，服务端轮询检测到数据变更后输出 hex dump。

## 代码结构

```
src/
├── lib.rs                 # 公共类型 + 辅助函数
│   ├── ConnInfo           # 控制通道交换的连接信息
│   ├── tcp_send_all/recv  # TCP 流式安全收发
│   ├── get_lid            # 查询端口 LID
│   └── get_port_mtu       # 查询端口 active MTU
└── bin/
    ├── rdma_server.rs     # RDMA 服务端（目标端）
    └── rdma_client.rs     # RDMA 客户端（发起端）
```

## 功能特点

- ✅ QP 状态机：RESET → INIT → RTR → RTS（Reliable Connection）
- ✅ RDMA Write 操作
- ✅ TCP 控制通道交换连接元数据
- ✅ CQ 轮询完成事件
- ✅ 跨架构字节序兼容（`ConnInfo::to_be/from_be`）
- ✅ TCP 流式处理（防止部分收发）
- ✅ 完整资源生命周期管理（无泄漏）

## 许可

MIT OR Apache-2.0

# Buddy Bridge

把本地 Ollama 大模型接到 M5StickC Plus 宠物上的 Rust 单文件工具。


Chrome 浏览器 <->buddy-bridge <-> M5StickC
 
│ 127.0.0.1:11500│  
 

---

## 特性

| 功能 | 说明 |
|---|---|
| 🚀 **单文件 exe** | 编译后一个文件，拷过去就能跑 |
| 💬 **内置聊天界面** | 不用装 Python，不用另开网页 |
| 🐷 **宠物实时响应** | M5Stick 上的猪显示 AI 状态 |
| 📎 **拖拽文件** | 拖代码/文件夹/图片直接问 |
| 📸 **截图问 AI** | Chrome 抓屏，发给视觉模型 |
| 🌐 **网页搜索** | 内置 DuckDuckGo 搜索 |
| 💻 **编程模式** | 聊天 / 编程 / 调试 / 讲解 |
| 📜 **历史记录** | localStorage 存最近 50 条对话 |
| 🎨 **代码高亮** | highlight.js + 一键复制 |
| ⌨️ **Ctrl+K 快捷命令** | 解释/重构/测试/修 bug |
| 😀 **情绪按钮** | 手动触发或 AI 自动驱动 |

---

## 硬件要求

| 项目 | 要求 |
|---|---|
| 系统 | Windows 10/11 |
| GPU | RTX 3060 12GB 或以上（推荐）|
| 内存 | 16 GB+ |
| M5Stick | M5StickC Plus (ESP32) |
| USB 线 | 支持数据传输的 USB-C |

### 模型显存参考（RTX 4070 12GB 实测）

| 模型 | 大小 | 用途 | 显存 |
|---|---|---|---|
| `qwen2.5:3b` | 1.9 GB | 快速问答 | ~2.5 GB |
| **`qwen3:8b`** | **5.2 GB** | **推荐主力** | **~6 GB** |
| `deepseek-coder-v2:16b` | 8.9 GB | 深度代码 | ~10 GB |
| `qwen2.5-vl:7b` | 4.7 GB | 看图片 | ~6 GB |

> 12GB 显存同一时间只能跑 1 个大模型。

---

## 快速开始

### 1. 装 Ollama

下载 https://ollama.com/download 安装。

```powershell
ollama --version

ollama pull qwen3:8b

M5Stick 需要先烧好匹配的固件。参考 claude-desktop-buddy 项目的 M5StickC Plus 版本，并加上 emote 解析。

powershell
cd C:\path\to\m5stick-firmware
pio run -e m5stickc-plus -t upload

# 构建环境说明（Windows / MSVC）

本机实测结论，务必遵守，否则 `cargo build`（debug）会在第三方 C 依赖上失败。

## 已确认的环境

| 项 | 值 |
|---|---|
| Rust 工具链 | `stable-x86_64-pc-windows-msvc` (rustc 1.99.1) |
| Visual Studio | VS 2026，安装于 `F:\vsiual studio` |
| MSVC 工具集 | 14.51.36231（另有 14.44.35207） |
| Windows SDK | `E:\WinPEAddons`（10.0.26100.0） |
| WebView2 Runtime | 149.0.4022.62（已安装） |
| cargo 镜像 | rsproxy.cn（`E:\Rust\cargo\config.toml`） |
| Node / pnpm | v24.16.0 / 10.4.1 |

## 必须注意的坑

### 1. debug 构建下 MSVC 的 `-Z7` 会失败

现象：任何通过 `cc` crate 编译 C 代码的构建脚本（`ring`、`libsqlite3-sys` 等）在
**debug profile** 下报：

```
cl.exe ... "-Z7" ... error: command did not execute successfully (status code exit code: 2)
```

原因：本机 MSVC 在写入调试信息（PDB）时失败，与 `cc`/`ring` 无关。手动执行同一条
命令（`cmd` 下）能成功，但在 `cargo` 构建脚本的子进程里必然失败。

**解决**：本项目的构建/验证一律使用 **release** profile：

```bash
cargo build --release
cargo check --release
pnpm tauri build -- --profile release
```

`tauri dev` 默认是 debug，因此本机 `tauri dev` 会失败——这是环境限制，不是项目缺陷。
需要在开发期快速验证时用：

```bash
pnpm tauri dev -- -- --profile release
```

或在 `src-tauri/.cargo/config.toml` 中固定 `[build] profile = "release"`（见下）。

### 2. GNU 工具链不可用（已弃用）

此前尝试 `x86_64-pc-windows-gnu`，缺少 `dlltool.exe`，`windows-sys` 无法生成导入库。
**不要切回 GNU 工具链。**

### 3. `link.exe` 冲突

`E:\Git\usr\bin\link.exe` 在 PATH 上，但 MSVC 目录在其之前。若出现链接异常，
先确认 `where link` 首项为 MSVC 的 `link.exe`。

### 4. PATH 污染

`E:\Zulu\zulu-*\bin`、`E:\Python314` 含 `vcruntime140.dll` / `msvcp140.dll`。
它们排在 MSVC 目录之后，一般不影响；若构建异常，可临时用最小 PATH。

## 推荐构建脚本

仓库内提供 `scripts/build-msvc.ps1`（见下），会自动：
1. 调用 `vcvars64.bat` 初始化 MSVC 环境
2. 把 MSVC bin 目录置于 PATH 最前
3. 以 release profile 执行 cargo / tauri 命令

## Linux 构建

Linux 侧不需要上述 workaround。依赖系统包：

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
  librsvg2-dev build-essential pkg-config libssl-dev
```

`cargo build --release` 与 `pnpm tauri build` 均可正常使用。

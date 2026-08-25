# Changelog

版本号规则:`主版本.次版本.修订号`(语义化版本)。每个 Release 附 Windows x64 一键包(zip 内含 exe + 模型,解压即用)。

## v0.1.0 — 2026-08-25

首个公开版本。

### 功能
- 全局热键呼出悬浮翻译条:打中文,实时显示英文,回车自动粘贴进原应用
- 完全离线:本地 opus-mt zh→en 模型(CTranslate2 int8 量化,oneDNN 后端),短句 ~30ms、长句 ~100ms
- 内置拼音输入(可 `Ctrl+P` 开关):无中文输入法的电脑直接打拼音,数字选词、空格选首选、回车整句转换(词频 Viterbi)
- 系统输入法完整支持:拼音内联组合、候选窗跟随光标
- 热键冲突自动降级:`Ctrl+Alt+Space` → `Ctrl+Shift+Space` → `Ctrl+Alt+E`,可用 `FASTRANS_HOTKEY` 指定
- 悬浮条可拖动(按住空白处),位置与设置持久化于 `%APPDATA%\fastrans\config.txt`
- `Ctrl+Q` 退出;剪贴板粘贴后自动还原

### 技术
- Rust 单二进制(egui 界面 + CTranslate2 引擎静态链接),无运行时依赖
- 引擎在工作线程加载并预热,启动约 1 秒内热键即可用
- 翻译竞速去重:过期结果不上屏,相同文本不重译

# SteamLinkMatcher

SteamLinkMatcher 是一个用于批量匹配 Steam 游戏本体链接的桌面工具。你可以粘贴或导入一批游戏名称，软件会搜索 Steam 商店并输出对应的 Steam 链接、App ID、BBCode 和 sframe。

当前主线版本是 `desktop/` 下的 Tauri 桌面版。旧 Python 网页原型已移动到 `legacy-web/`，仅作历史参考，不再作为主要维护版本。

本项目不是 Valve 或 Steam 官方产品，与 Valve Corporation 或 Steam 无关联。Steam 是 Valve Corporation 的商标。

## 主要功能

- 批量粘贴游戏名称。
- 导入 `.txt`、`.csv`、`.xlsx` 文件。
- `.csv` 和 `.xlsx` 默认读取第一列。
- TXT/CSV 自动兼容 UTF-8、UTF-8 BOM 和 GBK 编码。
- 自动清洗括号备注，例如到期时间。
- 中文名称使用简体中文 Steam 商店搜索，英文名称使用英文 Steam 商店搜索。
- 只匹配 Steam 游戏本体页面。
- 不确定时自动推荐最像结果并标记为需复核。
- 支持人工选择候选，或手动粘贴 Steam 链接修正。
- 手动粘贴链接后会自动读取 Steam 游戏名。
- 本地 SQLite 缓存人工确认、人工修正和搜索结果。
- 支持代理设置和 Steam 搜索间隔设置。
- 支持隐藏已匹配行，只显示需复核行。
- 支持批量确认需复核结果。
- 支持复制链接、复制 BBCode、复制 sframe。
- 支持导出 CSV / XLSX 明细。
- 支持队列打开所有链接，并调用系统默认浏览器。

## 使用方法

下载 Release 中的便携版压缩包，解压后运行：

```text
SteamLinkMatcher.exe
```

便携版会把缓存和设置保存在同目录的 `data/` 文件夹中。移动整个文件夹时，缓存也会一起带走。

## 导入格式

### TXT

每行一个游戏名：

```text
Corpse Keeper
Warhammer 40,000: Rogue Trader(2026.7.1到期）
黑神话:悟空
```

### CSV

默认读取第一列。支持 UTF-8、UTF-8 BOM、GBK 编码。

### XLSX

默认读取第一个工作表的 A 列。常见表头如 `游戏名`、`游戏名称`、`name` 会自动跳过。

## 复核与修正

当匹配结果不确定时，软件会标记为需复核。你可以：

- 点击 `复核`，从候选中选择正确游戏。
- 手动粘贴 Steam 游戏本体链接。
- 点击 `确认` 或 `确认全部复核`，把推荐结果写入人工确认缓存。

人工确认和人工修正会优先于自动搜索结果。

## 缓存

便携版缓存位于：

```text
data/cache.sqlite3
```

清理方式：

- `清搜索缓存`：只清空 Steam 搜索候选缓存，保留人工确认/修正记录。
- `清全部缓存`：清空搜索缓存，也清空人工确认/修正记录。

## 代理与限流

如果 Steam 搜索偶尔出现连接失败，可以在软件顶部设置 HTTP 代理，例如：

```text
http://127.0.0.1:7890
```

如果批量匹配时遇到 Steam 限流，可以把搜索间隔从默认 `1500ms` 调高到 `2500ms` 或 `3000ms`。

## 开发运行

进入桌面版项目：

```powershell
cd desktop
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
npm install
npm run tauri:dev
```

## 构建便携版

```powershell
cd desktop
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
npm run tauri:portable
```

构建完成后，可将：

```text
desktop/src-tauri/target/release/steam-link-matcher.exe
```

复制到便携目录，并在 exe 同目录保留：

```text
portable.flag
data/
```

## 旧版网页原型

旧 Python 网页版位于：

```text
legacy-web/
```

它只作为历史参考保留，后续功能默认在 Tauri 桌面版中维护。

## License

本项目使用 MIT License 发布，详见 `LICENSE`。

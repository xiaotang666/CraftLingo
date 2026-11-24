# CraftLingo

方块译匠 · CraftLingo 的**工具更新仓库**：只存放自动更新文件（`update.json`）与安装包 Release 资产，不存放应用源码。

增强词典数据已迁移到独立仓库：[xiaotang666/CraftLingo-Dictionary](https://github.com/xiaotang666/CraftLingo-Dictionary)。

## 发版方式

在源码目录执行一键发布：

```powershell
npm run release -- -Notes "更新说明" -Portable
```

脚本会打包、签名、生成 `update.json`，并在此仓库创建 `v<版本>` Release 上传：

- `CraftLingo_<版本>_x64-setup.exe`
- `CraftLingo_<版本>_x64-setup.exe.sig`
- `update.json`

## 客户端更新地址

```
https://github.com/xiaotang666/CraftLingo/releases/latest/download/update.json
```

因此正式更新必须发布为**普通 Release**（不要勾选 prerelease），否则不会成为 `latest`。

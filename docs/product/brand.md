# 图标与视觉资产

## 1. 母版与尺寸

- 唯一母版：`assets/icons/cairn_icon.svg`（1024×1024）。深色圆角底板 + 六瓣白色花标，保持灰度层次与轻微浮雕。
- 允许的光栅产物：
  - `cairn-32.png`、`cairn-64.png`、`cairn-128.png`、`cairn-256.png`
  - `cairn.ico`（包含 16/32/48/256）
  - `cairn-32.rgba`（托盘）
- 产物规格收敛于上述固定阶梯，均由母版统一渲染生成。
- `assets/icons/` 为全项目唯一样本位置：exe 图标、安装包、标题栏与托盘均统一引用该目录资产。

## 2. 使用位置

- 标题栏：32px PNG 按 **20px** 绘制，与 16px Cormorant SemiBold 字标并列。
- 托盘：`cairn-32.rgba`。
- PE/安装包：`cairn.ico`。
- 品牌资产更新时，直接替换母版并按固定阶梯重新导出。

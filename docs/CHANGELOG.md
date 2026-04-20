# Changelog

本文档记录所有值得用户关注的变更。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added

### Changed

- 页面不再显示普通成功或空结果提示，开始建索引、打开路径和无搜索结果时保持静默，仅保留异常信息展示。

### Deprecated

### Removed

### Fixed

- 索引图片时会自动跳过 AVIF 文件内容，避免伪装成 `.png` 等扩展名的 AVIF 图片被计为处理失败。

### Security

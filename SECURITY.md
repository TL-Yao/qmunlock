# Security policy

## Sensitive data

不要在 issue、Pull Request、日志、截图或测试文件中提交：

- `authst`、access token、refresh token、cookie 或 ekey；
- QQ 音乐本地数据库、plist、进程内存转储；
- 可识别帐号、歌曲或下载来源的真实文件。
- 包含用户名、音乐文件路径的 `processing-history.json` 或 WebView 路径偏好导出。

处理历史仅保存在本机应用数据目录，最多保留 10,000 条；载入和批次结束时清理输出已删除的记录。设置中的「清除处理记录」会删除历史文件及缓存，目录偏好仍保留在 WebView localStorage。曲库匹配、命名元数据、封面、歌词及自动 ekey 获取会联网；音频内容不会上传，历史不保存 ekey 或登录令牌。

如果不慎提交了凭据，请立即在对应服务中使其失效，并从 Git 历史中清除；仅删除工作区文件不够。

## Reporting a vulnerability

在仓库启用 GitHub Security Advisories 后，请用私密报告功能提交漏洞。启用前，请不要公开披露可利用的细节；通过仓库所有者提供的私密联系方式报告，并附上复现步骤、影响范围和修复建议。

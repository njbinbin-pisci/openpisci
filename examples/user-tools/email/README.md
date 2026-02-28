# Email User Tool（TypeScript / Deno 参考实现）

这是 Pisci Desktop **用户工具插件系统**的参考实现，演示了：

- 如何用 TypeScript（Deno 运行时）编写用户工具
- 如何声明 `config_schema` 让 Pisci 自动生成配置表单
- 如何在工具脚本中读取注入的配置（SMTP/IMAP 凭据）
- 如何输出 Pisci 期望的 JSON 格式

---

## 安装到 Pisci

1. 在 Pisci Desktop 中切换到 **用户工具（🔧）** 标签页
2. 在「安装工具」输入框中粘贴本目录的**绝对路径**，例如：
   ```
   C:\Users\you\path\to\examples\user-tools\email
   ```
   或者，如果你把整个 `email/` 目录打包成 zip 后上传到某个 URL，也可以填写 HTTPS URL。
3. 点击「安装」
4. 安装后点击「配置」填写 SMTP / IMAP 信息并保存

> 首次安装需要已安装 [Deno](https://deno.land/) — 在 PowerShell 中执行：
> ```powershell
> winget install DenoLand.Deno
> ```

---

## 工具结构

```
email/
├── manifest.json   ← 工具元数据、输入 schema、配置 schema
└── index.ts        ← 工具脚本入口（TypeScript）
```

### manifest.json 字段说明

| 字段 | 说明 |
|------|------|
| `name` | 工具唯一标识（也是注册到 LLM 的工具名） |
| `description` | LLM 看到的工具描述 |
| `runtime` | `"deno"` / `"node"` / `"powershell"` / `"python"` |
| `entrypoint` | 相对于工具目录的入口文件 |
| `input_schema` | JSON Schema，定义 LLM 可传入的参数 |
| `config_schema` | 用户在 Pisci 设置页填写的字段，`type: "password"` 自动加密 |
| `timeout_secs` | 子进程最大运行时间（默认 60s） |

---

## 调用协议

Pisci 以子进程方式调用工具：

```
deno run --allow-all index.ts '<input_json>' '<config_json>'
```

- `argv[1]` = LLM 传入的参数（JSON 字符串）
- `argv[2]` = 用户在 Pisci 设置的配置（JSON 字符串，**密码字段为明文**，仅在进程内存中，不写日志）

脚本必须向 **stdout** 输出单行 JSON：

```json
// 成功
{ "ok": true, "content": "Email sent successfully." }

// 失败
{ "ok": false, "error": "SMTP authentication failed" }
```

---

## 支持的操作

| `action` | 功能 | 必填参数 |
|----------|------|---------|
| `send` | 发送邮件 | `to`, `subject`, `body` |
| `fetch` | 获取收件箱最近邮件 | — (`limit` 可选) |
| `search` | 按主题关键词搜索 | `query` |

---

## 配置字段

| 字段 | 说明 | 示例 |
|------|------|------|
| `smtp_host` | SMTP 服务器 | `smtp.gmail.com` |
| `smtp_port` | SMTP 端口 | `587`（STARTTLS）/ `465`（SSL） |
| `smtp_username` | 邮箱账号 | `you@gmail.com` |
| `smtp_password` | 密码或应用专用密码 | Gmail 需生成 App Password |
| `imap_host` | IMAP 服务器 | `imap.gmail.com` |
| `imap_port` | IMAP 端口 | `993` |
| `from_name` | 发件人显示名 | `My AI Agent`（可选） |

---

## 在 LLM 对话中使用

安装并配置好后，直接在对话中说：

```
帮我给 bob@example.com 发一封主题为「测试」的邮件，内容是「你好，这是一封测试邮件」
```

或：

```
查一下我最近收到的10封邮件
```

LLM 会自动调用 `email` 工具，Pisci 将凭据安全地注入给脚本执行。

---

## 扩展示例

你可以复制本目录作为模板来创建自己的工具。只需修改：

1. `manifest.json` — 改名字、描述、schema
2. `index.ts` — 实现业务逻辑

任何支持从 `argv` 读取参数并向 `stdout` 输出 JSON 的语言都可以作为用户工具运行时。

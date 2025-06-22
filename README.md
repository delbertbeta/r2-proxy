# R2 代理服务器

一个高性能的 Rust S3 代理服务器，用于代理 Cloudflare R2 存储的请求。

## 功能特性

- 🔒 **白名单验证**: 通过 Cloudflare KV 验证 bucket 访问权限
- 🌐 **CORS 支持**: 动态从 KV 获取并应用 CORS 配置
- ⚡ **高性能**: 支持流式传输，节省内存和存储资源
- 🛡️ **安全**: 只允许 GET 和 OPTIONS 请求
- 📊 **日志**: 完整的请求日志和错误追踪

## 环境变量配置

创建 `.env` 文件并配置以下环境变量：

```env
# 服务器配置
PORT=3000

# Cloudflare 配置
CLOUDFLARE_ACCOUNT_ID=your_cloudflare_account_id
CLOUDFLARE_API_TOKEN=your_cloudflare_api_token
CLOUDFLARE_KV_NAMESPACE_ID=your_kv_namespace_id

# Cloudflare R2 配置
R2_ENDPOINT=https://your-account-id.r2.cloudflarestorage.com
R2_ACCESS_KEY_ID=your_r2_access_key_id
R2_SECRET_ACCESS_KEY=your_r2_secret_access_key
```

## Cloudflare KV 配置

### 白名单配置

在 KV 中存储 bucket 白名单，键格式：`whitelist:{bucket_name}`，值为 `true` 或 `false`。

示例：
```
whitelist:my-bucket = true
whitelist:another-bucket = false
```

### CORS 配置

在 KV 中存储 CORS 配置，键格式：`cors:{bucket_name}`，值为 JSON 格式的 CORS 配置。

示例：
```json
{
  "allowed_origins": ["*"],
  "allowed_methods": ["GET", "OPTIONS"],
  "allowed_headers": ["*"],
  "expose_headers": [],
  "max_age": 86400,
  "allow_credentials": false
}
```

## Bucket Mapping

- 访问 mybucket.delbertbeta.life/path 代表虚拟 bucket "mybucket"，访问 delbertbeta.life 代表虚拟 bucket "@"
- 实际访问的 S3 bucket 由 KV 中 whitelist 的 value 决定。例如：
  - `whitelist` = `[["foo", "real-bucket-name"], ["@", "default-bucket"]]`，则 foo.delbertbeta.life 代理到 S3 的 real-bucket-name，delbertbeta.life 代理到 default-bucket
- CORS 配置由 KV 中 cors 的 value 决定，格式为 `{ "bucketname": { ...CORS配置... } }`
- 如果没有配置或 value 为空，则拒绝访问

## API Usage

### Request Format

```
GET https://[bucket].delbertbeta.life/{path}
```

### Example Request

```bash
# Get file
curl https://my-bucket.delbertbeta.life/images/logo.png

# Preflight request
curl -X OPTIONS https://my-bucket.delbertbeta.life/images/logo.png
```

## 构建和运行

### 开发环境

```bash
# 安装依赖
cargo build

# 运行开发服务器
cargo run
```

### 生产环境

```bash
# 构建发布版本
cargo build --release

# 运行生产服务器
./target/release/r2-proxy
```

## 项目结构

```
src/
├── main.rs          # 主程序入口
├── config.rs        # 配置管理
├── errors.rs        # 错误处理
├── cors.rs          # CORS 配置
├── kv_client.rs     # Cloudflare KV 客户端
└── s3_client.rs     # S3/R2 客户端
```

## 性能优化

- 使用流式传输减少内存占用
- 异步处理提高并发性能
- 连接池复用减少连接开销
- 错误缓存避免重复请求

## 安全考虑

- 只允许 GET 和 OPTIONS 请求
- 通过 KV 白名单验证 bucket 访问权限
- 动态 CORS 配置防止跨域攻击
- 完整的错误处理和日志记录

## 许可证

MIT License

## Docker Usage

### Build image (if local build)
```bash
docker build -t r2-proxy .
```

### Run with .env file (recommended)
```bash
docker run -d -p 3000:3000 --name r2-proxy --restart unless-stopped --volume .env:/app/.env delbertbeta/r2-proxy:latest
```

### 或直接用环境变量
```bash
docker run --rm -p 3000:3000 -e PORT=3000 -e CLOUDFLARE_ACCOUNT_ID=xxx ... delbertbeta/r2-proxy:latest
``` 
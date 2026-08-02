## ADDED Requirements

### Requirement: session + cookie + CSRF + API key 四种认证方式 SHALL
The system SHALL satisfy the following behavior.

`pc-auth` 实现 session（cookie `paperclip.session` + DB 存储）、CSRF（`x-paperclip-csrf` 头）、API key（`x-paperclip-agent-key` 头，hash 比对）、board 用户/agent 双主体。

#### Scenario: 登录获取 session
- **WHEN** `POST /api/auth/sign-in` 合法凭据
- **THEN** 设置 `Set-Cookie: paperclip.session=...; HttpOnly; SameSite=Lax` + 返回 200

#### Scenario: CSRF 缺失
- **WHEN** `POST /api/...` 缺 `x-paperclip-csrf`
- **THEN** 返回 403

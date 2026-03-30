# Future integrations

Exploratory notes for connecting Mervyn to corporate Microsoft 365, WhatsApp, and Facebook Messenger. Not a commitment or implementation plan. For current architecture, see [mervyn_project_spec.md](mervyn_project_spec.md).

---

## Microsoft 365 (mail and calendar)

**Likely approach:** [Microsoft Graph](https://learn.microsoft.com/en-us/graph/overview) with an app registration in the corporate tenant (or a multi-tenant app subject to admin consent).

| Capability | Graph direction | Typical permissions (high level) |
|------------|-----------------|----------------------------------|
| Read mail | Messages, folders, [delta query](https://learn.microsoft.com/en-us/graph/delta-query-messages) for incremental sync | `Mail.Read` (read-only) or `Mail.ReadWrite` if sending is required |
| Read/write calendar | Calendars, events, free/busy | `Calendars.Read` / `Calendars.ReadWrite` |
| Send email | `sendMail` and related APIs | `Mail.Send` or combined mail write scopes |
| Schedule email | Product “schedule send” is mostly a client feature | Model in Mervyn: persist “send at” + cron/job that calls Graph send at that time |

**Corporate constraints:** Conditional Access, MFA, IP allowlists, and **admin approval** for the app and delegated vs application permissions. Work/school (Entra ID) tenants differ from personal Microsoft accounts; expect tenant-specific configuration and security review.

**Fit with Mervyn:** OAuth with refresh tokens (delegated user), or app-only access to specific mailboxes if IT permits. Webhooks ([Graph change notifications](https://learn.microsoft.com/en-us/graph/webhooks)) can reduce polling for mail/calendar updates.

---

## WhatsApp

**Official channel:** [WhatsApp Business Platform / Cloud API](https://developers.facebook.com/docs/whatsapp/cloud-api) — aimed at **business** messaging (customers messaging a business number), not a supported “read my personal WhatsApp like the consumer app” API.

- **Business inbox (official):** Reading and sending for the **business** number is in scope for the Cloud API and aligns with Meta’s product model.
- **Personal WhatsApp:** Full read/send “as me” through a stable public API is generally **not** available. Unofficial approaches imply ToS risk, account suspension, security issues, and frequent breakage.

**Implication:** Treat “WhatsApp” as **Business API only** unless the project explicitly accepts personal-client constraints.

---

## Facebook Messenger

**Official channel:** [Messenger Platform](https://developers.facebook.com/docs/messenger-platform) — strong for **Facebook Pages** (page inbox, automated handling), not a full replacement for a personal Messenger client API.

- **Send/receive as the Page** is the normal, supported pattern.
- **Personal Messenger** as a first-class “read everything / send as personal user” integration is not something to rely on for a durable assistant design.

---

## Summary

| Surface | Read (realistic) | Write “as you” (realistic) | Notes |
|---------|------------------|----------------------------|--------|
| M365 mail / calendar | Yes, via Graph | Yes (mail, events) with correct scopes | Tenant IT and consent are the gates |
| WhatsApp | Business number via Cloud API | Same | Personal WhatsApp: unsupported / high risk |
| Messenger | Page conversations via Platform | As Page | Personal inbox ≠ same API story |

---

## Implementation themes (when/if pursued)

- **Auth:** OAuth authorization code or device code flow; secure storage of refresh tokens; minimize scopes.
- **Ingress:** Graph subscriptions vs polling; rate limits and retries.
- **Scheduling:** Mervyn’s existing cron/scheduler can own “send later” for email if Graph does not expose tenant-specific schedule-send.
- **Safety:** Separate connector modules, explicit capability flags in config, and audit-friendly logging for any send/write path.

---

## Related documents

- [mervyn_project_spec.md](mervyn_project_spec.md) — architecture, Slack, vault, Claude, deployment
- [README.md](README.md) — quick start and current integrations

/**
 * Pisci User Tool: Email (TypeScript / Deno reference implementation)
 *
 * Protocol:
 *   argv[1] = JSON string of LLM tool input
 *   argv[2] = JSON string of user config (SMTP/IMAP credentials)
 *
 * Output:  JSON { ok: true, content: "..." } or { ok: false, error: "..." }
 *
 * Runtime: Deno 1.x+ (deno run --allow-all index.ts)
 *
 * Dependencies (auto-fetched by Deno):
 *   - smtp:  npm:nodemailer
 *   - imap:  Deno-native TCP + IMAP protocol (simple implementation)
 *
 * Note: For a production-grade IMAP client consider using an npm package.
 */

import { createTransport } from "npm:nodemailer@6";

// ─── Types ───────────────────────────────────────────────────────────────────

interface ToolInput {
  action: "send" | "fetch" | "search";
  to?: string;
  subject?: string;
  body?: string;
  limit?: number;
  query?: string;
}

interface ToolConfig {
  smtp_host: string;
  smtp_port: number;
  smtp_username: string;
  smtp_password: string;
  imap_host: string;
  imap_port: number;
  from_name?: string;
}

// ─── Output helper ───────────────────────────────────────────────────────────

function ok(content: string): never {
  console.log(JSON.stringify({ ok: true, content }));
  Deno.exit(0);
}

function fail(error: string): never {
  console.log(JSON.stringify({ ok: false, error }));
  Deno.exit(1);
}

// ─── SMTP: send email ─────────────────────────────────────────────────────────

async function sendEmail(input: ToolInput, cfg: ToolConfig): Promise<string> {
  if (!input.to)      fail("'to' is required for action=send");
  if (!input.subject) fail("'subject' is required for action=send");
  if (!input.body)    fail("'body' is required for action=send");

  if (!cfg.smtp_host || !cfg.smtp_username || !cfg.smtp_password) {
    fail("SMTP credentials not configured. Open User Tools → Configure to set them.");
  }

  const transporter = createTransport({
    host: cfg.smtp_host,
    port: cfg.smtp_port ?? 587,
    secure: (cfg.smtp_port ?? 587) === 465,
    auth: {
      user: cfg.smtp_username,
      pass: cfg.smtp_password,
    },
  });

  const info = await transporter.sendMail({
    from: cfg.from_name
      ? `"${cfg.from_name}" <${cfg.smtp_username}>`
      : cfg.smtp_username,
    to: input.to,
    subject: input.subject,
    text: input.body,
  });

  return `Email sent successfully. Message-ID: ${info.messageId}`;
}

// ─── IMAP: fetch recent emails ────────────────────────────────────────────────

/**
 * Minimal IMAP client using Deno's TCP.
 * For production use, replace with a proper IMAP library.
 */
async function fetchEmails(
  cfg: ToolConfig,
  limit: number,
  searchQuery?: string,
): Promise<string> {
  if (!cfg.imap_host || !cfg.smtp_username || !cfg.smtp_password) {
    fail("IMAP credentials not configured. Open User Tools → Configure to set them.");
  }

  // Use nodemailer's IMAP is not included; we do a simple raw IMAP exchange.
  // For a richer experience install npm:imap or npm:imapflow
  try {
    const { ImapFlow } = await import("npm:imapflow@1");

    const client = new ImapFlow({
      host: cfg.imap_host,
      port: cfg.imap_port ?? 993,
      secure: true,
      auth: {
        user: cfg.smtp_username,
        pass: cfg.smtp_password,
      },
      logger: false,
    });

    await client.connect();
    const lock = await client.getMailboxLock("INBOX");
    const messages: string[] = [];

    try {
      const criteria = searchQuery
        ? { header: { Subject: searchQuery } }
        : { all: true };

      const uids: number[] = await client.search(criteria, { uid: true });
      const recentUids = uids.slice(-Math.min(limit, 50));

      for await (const msg of client.fetch(recentUids, {
        envelope: true,
        uid: true,
      })) {
        const env = msg.envelope;
        const from = env.from?.[0]
          ? `${env.from[0].name ?? ""} <${env.from[0].address}>`.trim()
          : "unknown";
        messages.push(
          `[${msg.uid}] ${env.date?.toISOString().slice(0, 10) ?? "?"} | From: ${from} | Subject: ${env.subject ?? "(no subject)"}`,
        );
      }
    } finally {
      lock.release();
      await client.logout();
    }

    if (messages.length === 0) {
      return searchQuery
        ? `No emails found matching "${searchQuery}"`
        : "Inbox is empty";
    }
    return messages.join("\n");
  } catch (e) {
    fail(`IMAP error: ${e instanceof Error ? e.message : String(e)}`);
  }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

async function main() {
  if (Deno.args.length < 2) {
    fail("Usage: deno run index.ts '<input_json>' '<config_json>'");
  }

  let input: ToolInput;
  let config: ToolConfig;

  try {
    input = JSON.parse(Deno.args[0]) as ToolInput;
  } catch {
    fail("Invalid input JSON");
  }

  try {
    config = JSON.parse(Deno.args[1]) as ToolConfig;
  } catch {
    fail("Invalid config JSON");
  }

  switch (input.action) {
    case "send":
      ok(await sendEmail(input, config));
      break;
    case "fetch":
      ok(await fetchEmails(config, input.limit ?? 10));
      break;
    case "search":
      ok(await fetchEmails(config, input.limit ?? 10, input.query));
      break;
    default:
      fail(`Unknown action: ${(input as { action: string }).action}`);
  }
}

main().catch((e) => fail(String(e)));

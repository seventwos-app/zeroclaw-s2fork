/**
 * ZeroClaw Proxy Worker
 *
 * Routes chat requests based on credential type:
 *   1. OAuth token (sk-ant-oat01-*) — api.anthropic.com/v1/messages with x-api-key
 *   2. Session key (sk-ant-sid0*) — claude.ai web API (org + conversation flow)
 *
 * Endpoints:
 *   POST /chat   — { "message": "...", "system"?: "...", "model"?: "..." }
 *   GET  /health — health check
 *
 * Secrets: CLAUDE_SESSION_KEY (OAuth token or session key)
 */

interface Env {
  CLAUDE_SESSION_KEY: string;
  GATEWAY_TOKEN?: string;
  ALLOWED_ORIGIN: string;
}

interface ChatRequest {
  message: string;
  system?: string;
  model?: string;
  conversation_id?: string;
}

const ANTHROPIC_API_BASE = "https://api.anthropic.com";
const CLAUDE_WEB_BASE = "https://claude.ai/api";
const DEFAULT_MODEL = "claude-sonnet-4-6";

function corsHeaders(origin: string, allowedOrigin: string): HeadersInit {
  return {
    "Access-Control-Allow-Origin": allowedOrigin === "*" ? "*" : origin,
    "Access-Control-Allow-Methods": "POST, GET, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type, Authorization",
  };
}

function isOAuthToken(key: string): boolean {
  return key.startsWith("sk-ant-oat01-");
}

function isSessionKey(key: string): boolean {
  return key.startsWith("sk-ant-sid01-") || key.startsWith("sk-ant-sid02-");
}

// ── OAuth token path: api.anthropic.com ─────────────────────

async function chatViaApi(
  token: string,
  message: string,
  system: string | undefined,
  model: string
): Promise<string> {
  const messages = [{ role: "user", content: message }];
  const body: Record<string, unknown> = {
    model,
    max_tokens: 4096,
    messages,
  };
  if (system) {
    body.system = system;
  }

  const res = await fetch(`${ANTHROPIC_API_BASE}/v1/messages`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "anthropic-beta": "oauth-2025-04-20",
      "anthropic-version": "2023-06-01",
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });

  if (!res.ok) {
    const errBody = await res.text();
    throw new Error(`Anthropic API error (${res.status}): ${errBody.slice(0, 300)}`);
  }

  const data = (await res.json()) as {
    content: { type: string; text?: string }[];
  };
  const text = data.content?.find((c) => c.type === "text")?.text;
  if (!text) throw new Error("No text in Anthropic API response");
  return text.trim();
}

// ── Session key path: claude.ai web API ─────────────────────

function sessionHeaders(sessionKey: string): HeadersInit {
  return {
    Cookie: `sessionKey=${sessionKey}`,
    "User-Agent":
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Content-Type": "application/json",
  };
}

async function getOrgId(sessionKey: string): Promise<string> {
  const res = await fetch(`${CLAUDE_WEB_BASE}/organizations`, {
    headers: sessionHeaders(sessionKey),
  });
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`Org fetch failed (${res.status}): ${body.slice(0, 200)}`);
  }
  const orgs = (await res.json()) as { uuid: string }[];
  if (!orgs.length) throw new Error("No organizations found");
  return orgs[0].uuid;
}

async function chatViaSession(
  sessionKey: string,
  prompt: string,
  model: string,
  conversationId?: string
): Promise<string> {
  const orgId = await getOrgId(sessionKey);
  const headers = sessionHeaders(sessionKey);

  const convId = conversationId || crypto.randomUUID();
  if (!conversationId) {
    const createRes = await fetch(
      `${CLAUDE_WEB_BASE}/organizations/${orgId}/chat_conversations`,
      {
        method: "POST",
        headers,
        body: JSON.stringify({ name: "", uuid: convId }),
      }
    );
    if (!createRes.ok) {
      const body = await createRes.text();
      throw new Error(
        `Conversation create failed (${createRes.status}): ${body.slice(0, 200)}`
      );
    }
  }

  const completionRes = await fetch(
    `${CLAUDE_WEB_BASE}/organizations/${orgId}/chat_conversations/${convId}/completion`,
    {
      method: "POST",
      headers: { ...headers, Accept: "text/event-stream" },
      body: JSON.stringify({
        prompt,
        timezone: "America/New_York",
        attachments: [],
        files: [],
        model,
        rendering_mode: "raw",
      }),
    }
  );

  if (!completionRes.ok) {
    const body = await completionRes.text();
    throw new Error(
      `Completion failed (${completionRes.status}): ${body.slice(0, 200)}`
    );
  }

  const text = await completionRes.text();
  let result = "";
  for (const line of text.split("\n")) {
    if (!line.startsWith("data: ")) continue;
    try {
      const d = JSON.parse(line.slice(6));
      if (d.type === "completion" && d.completion) {
        result += d.completion;
      }
    } catch {
      // skip non-JSON lines
    }
  }

  if (!result) throw new Error("Empty response from Claude");
  return result.trim();
}

// ── Request handler ─────────────────────────────────────────

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const cors = corsHeaders(
      request.headers.get("Origin") || "*",
      env.ALLOWED_ORIGIN
    );

    if (request.method === "OPTIONS") {
      return new Response(null, { status: 204, headers: cors });
    }

    if (url.pathname === "/health") {
      return Response.json(
        { status: "ok", service: "zeroclaw-proxy" },
        { headers: cors }
      );
    }

    if (url.pathname === "/chat" && request.method === "POST") {
      if (env.GATEWAY_TOKEN) {
        const auth = request.headers.get("Authorization");
        if (auth !== `Bearer ${env.GATEWAY_TOKEN}`) {
          return Response.json(
            { error: "Unauthorized" },
            { status: 401, headers: cors }
          );
        }
      }

      if (!env.CLAUDE_SESSION_KEY) {
        return Response.json(
          { error: "CLAUDE_SESSION_KEY not configured" },
          { status: 500, headers: cors }
        );
      }

      try {
        const body = (await request.json()) as ChatRequest;
        if (!body.message) {
          return Response.json(
            { error: "message is required" },
            { status: 400, headers: cors }
          );
        }

        const model = body.model || DEFAULT_MODEL;
        const credential = env.CLAUDE_SESSION_KEY;
        let reply: string;

        if (isOAuthToken(credential)) {
          // OAuth token → api.anthropic.com with x-api-key (no org fetch needed)
          reply = await chatViaApi(credential, body.message, body.system, model);
        } else if (isSessionKey(credential)) {
          // Session key → claude.ai web API (org + conversation flow)
          let prompt = "";
          if (body.system) {
            prompt += body.system + "\n\n---\n\n";
          }
          prompt += body.message;
          reply = await chatViaSession(
            credential,
            prompt,
            model,
            body.conversation_id
          );
        } else {
          // Assume standard API key → api.anthropic.com with x-api-key
          reply = await chatViaApi(credential, body.message, body.system, model);
        }

        return Response.json({ response: reply }, { headers: cors });
      } catch (err: unknown) {
        const message =
          err instanceof Error ? err.message : "Unknown error";
        return Response.json(
          { error: message },
          { status: 502, headers: cors }
        );
      }
    }

    return Response.json(
      { error: "Not found. Use POST /chat or GET /health" },
      { status: 404, headers: cors }
    );
  },
};

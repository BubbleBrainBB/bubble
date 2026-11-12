/**
 * Completion check for chat-completions SSE replies.
 *
 * An SSE body carries no length of its own: a reply that stops early looks
 * the same to the SSE parser as one that finished. The only proof that a
 * chat-completions reply is whole is its own terminal marker, a
 * `data: [DONE]` line or a chunk with a non-null `finish_reason`. A body that
 * ends without either was cut off somewhere between the model and Bubble, so
 * it fails here instead of being handed on as a finished answer.
 *
 * The check sits on the fetch because the OpenAI SDK swallows `[DONE]` before
 * the provider code sees any chunk.
 */

import type { ProviderFetch } from "./provider-transport.js";

export class ChatStreamIncompleteError extends Error {
  constructor() {
    super("the reply ended without [DONE] or a finish_reason");
    this.name = "ChatStreamIncompleteError";
  }
}

export function withChatStreamCompletionCheck(fetchImpl: ProviderFetch): ProviderFetch {
  return async (input, init) => {
    const response = await fetchImpl(input, init);
    if (!response.ok || !response.body) return response;
    if (!/\btext\/event-stream\b/i.test(response.headers.get("content-type") ?? "")) return response;
    return new Response(response.body.pipeThrough(createCompletionCheck()), {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    });
  };
}

function createCompletionCheck(): TransformStream<Uint8Array, Uint8Array> {
  const decoder = new TextDecoder();
  let unfinishedLine = "";
  let complete = false;

  return new TransformStream<Uint8Array, Uint8Array>({
    transform(chunk, controller) {
      controller.enqueue(chunk);
      if (complete) return;
      const lines = (unfinishedLine + decoder.decode(chunk, { stream: true })).split(/\r\n|\n|\r/);
      unfinishedLine = lines.pop() ?? "";
      complete = lines.some(isTerminalLine);
    },
    flush(controller) {
      if (complete || isTerminalLine(unfinishedLine + decoder.decode())) return;
      controller.error(new ChatStreamIncompleteError());
    },
  });
}

function isTerminalLine(line: string): boolean {
  if (!line.startsWith("data:")) return false;
  const data = line.slice("data:".length).trimStart();
  // Quotes inside a JSON string value are escaped, so model text that merely
  // mentions finish_reason cannot match.
  return data.startsWith("[DONE]") || /"finish_reason"\s*:\s*"/.test(data);
}

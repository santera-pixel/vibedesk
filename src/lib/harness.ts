/**
 * Telling the harness apart from the model.
 *
 * The agent uses `agent_message_chunk` for two different things: the model's
 * reply, and its own startup diagnostics. A connector manifest that fails to
 * fetch arrives as an assistant message — "Couldn't load your connectors:
 * Failed to load connectors (HTTP 502)" — which is how a transient upstream
 * failure ended up as the assistant's opening line in every new chat.
 *
 * A reply requires a prompt, so the distinction is structural rather than
 * textual: if nothing has been said in the session yet, the agent is not
 * replying to anything. Matching the wording instead would break the moment
 * the CLI rephrased the message, and would also swallow a real reply that
 * happened to quote it.
 *
 * The check is deliberately narrow — no user turn *and* no agent turn. A
 * transcript that has been trimmed keeps its tail, so a long chat always has
 * both and can never be mistaken for a fresh one.
 */

/** The least a transcript item has to expose for the check to work. */
export interface Speaker {
  type: string;
}

/**
 * Whether an agent message that just arrived is the harness talking.
 *
 * True only before the session has said anything at all. Once a user turn
 * exists the agent is answering it, and once an agent turn exists the session
 * has already started speaking, so neither can be a startup diagnostic.
 */
export function isStartupDiagnostic(items: ReadonlyArray<Speaker>): boolean {
  return !items.some((item) => item.type === 'user' || item.type === 'agent');
}

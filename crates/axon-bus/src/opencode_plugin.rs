//! The OpenCode plugin shim: a Bun module that forwards session and tool events to
//! `axon bus hook opencode …`, turns a deny reply into a thrown error, and appends bus
//! context (the introduction, peer messages) to a tool's output.

use serde_json::json;

use crate::install::bus_verb;

pub(crate) fn plugin_source(exe: &str) -> String {
    let hook = if bus_verb(exe).is_empty() {
        json!([exe, "hook"])
    } else {
        json!([exe, "bus", "hook"])
    };
    format!(
        r#"// axon-bus plugin shim; `axon-bus uninstall` removes it.
const HOOK = {hook};
function hook(event, payload) {{
  const run = Bun.spawnSync([...HOOK, "opencode", event], {{
    stdin: Buffer.from(JSON.stringify(payload)),
  }});
  try {{
    const out = run.stdout.toString().trim();
    return out ? JSON.parse(out) : null;
  }} catch {{
    return null;
  }}
}}
export const AxonBus = async () => ({{
  event: async ({{ event }}) => {{
    if (["session.created", "session.idle", "session.deleted"].includes(event.type)) hook(event.type, event);
  }},
  "tool.execute.before": async (input, output) => {{
    const reply = hook("tool.execute.before", {{ input, output }});
    if (reply?.decision === "deny") throw new Error(reply.reason);
  }},
  "tool.execute.after": async (input, output) => {{
    const reply = hook("tool.execute.after", {{ input, output }});
    if (reply?.context && typeof output.output === "string") output.output += "\n\n" + reply.context;
  }},
}});
"#
    )
}

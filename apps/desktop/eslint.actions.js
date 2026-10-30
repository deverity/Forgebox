import { readFileSync } from "node:fs";

// Mutating API methods, read from the generated method table (cargo xtask gen-ts) so the rule
// never drifts from forge_ipc::api::METHODS.
const TABLE = new URL("./src/ipc/methods.ts", import.meta.url);

/** @returns {Set<string>} names of the methods with `mutating: true`. */
export function mutatingMethods() {
  const names = new Set();
  for (const line of readFileSync(TABLE, "utf8").split("\n")) {
    const match = /^\s*"([a-z_.]+)": \{ kind: "[a-z]+",.*\bmutating: true\b/.exec(line);
    if (match?.[1]) names.add(match[1]);
  }
  return names;
}

/** @type {import("eslint").Rule.RuleModule} */
const noDirectMutation = {
  meta: {
    type: "problem",
    schema: [],
    messages: {
      direct:
        "'{{method}}' mutates state: define it with defineAction() in src/actions and run it through <ActionButton> or runAction().",
    },
  },
  create(context) {
    const mutating = mutatingMethods();
    return {
      CallExpression(node) {
        const callee = node.callee;
        if (callee.type !== "MemberExpression" || callee.computed) return;
        if (callee.property.type !== "Identifier") return;
        if (callee.property.name !== "call" && callee.property.name !== "job") return;
        const first = node.arguments[0];
        let method = null;
        if (first?.type === "Literal" && typeof first.value === "string") method = first.value;
        else if (first?.type === "TemplateLiteral" && first.expressions.length === 0) {
          method = first.quasis[0]?.value.cooked ?? null;
        }
        if (method !== null && mutating.has(method)) {
          context.report({ node: first, messageId: "direct", data: { method } });
        }
      },
    };
  },
};

/**
 * Flat-config block shared by `eslint.config.js` and the fixture test (E4-T5 parity gate:
 * every mutation of the GUI is a registered action, hence has its `forge` command).
 * Infrastructure that forwards arbitrary methods (ipc, state) and tests are exempt.
 * @type {import("eslint").Linter.Config}
 */
export const actionsConfig = {
  files: ["src/**/*.{ts,tsx}"],
  ignores: ["src/actions/**", "src/ipc/**", "src/state/**", "src/**/*.test.{ts,tsx}"],
  plugins: { forgebox: { rules: { "no-direct-mutation": noDirectMutation } } },
  rules: { "forgebox/no-direct-mutation": "error" },
};

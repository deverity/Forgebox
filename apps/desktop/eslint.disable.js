// Every ESLint rule exemption needs an inline reason (docs/07 section 11, M2-E4-T3):
//   // eslint-disable-next-line some/rule -- why this is safe here
// ESLint's own description separator is ` -- `; a directive without text after it fails lint.
const DIRECTIVE = /^\s*eslint-disable(?:-next-line|-line)?(?![\w-])(.*)$/s;

/** @type {import("eslint").Rule.RuleModule} */
const disableNeedsReason = {
  meta: {
    type: "problem",
    schema: [],
    messages: {
      missing:
        "eslint-disable needs an inline reason: add ' -- <why this exemption is justified>'.",
    },
  },
  create(context) {
    return {
      Program() {
        for (const comment of context.sourceCode.getAllComments()) {
          const match = DIRECTIVE.exec(comment.value);
          if (match === null) continue;
          const separator = /(?:^|\s)--(?:\s+|$)(.*)$/s.exec(match[1] ?? "");
          if ((separator?.[1] ?? "").trim() === "") {
            context.report({ loc: comment.loc ?? { line: 1, column: 0 }, messageId: "missing" });
          }
        }
      },
    };
  },
};

/**
 * Flat-config block shared by `eslint.config.js` and the fixture test. Also fails directives
 * that no longer suppress anything (see `linterOptions` in eslint.config.js).
 * @type {import("eslint").Linter.Config}
 */
export const disableReasonConfig = {
  plugins: { "forgebox-disable": { rules: { "needs-reason": disableNeedsReason } } },
  rules: { "forgebox-disable/needs-reason": "error" },
};

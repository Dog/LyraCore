const ts = require("typescript");
const tstl = require("typescript-to-lua");
const lua = require("typescript-to-lua/dist/LuaAST");

const GUARD = "____tbl";

const ENTRY = "script";

// Piccolo 0.3.3 leaks a trailing table constructor's element count as an extra argument.
function guardTrailingTable(params) {
  if (!params || params.length === 0) return params;
  const last = params[params.length - 1];
  if (!lua.isTableExpression(last)) return params;
  const wrapped = lua.createCallExpression(lua.createIdentifier(GUARD), [last]);
  return [...params.slice(0, -1), wrapped];
}

function guardDeclaration() {
  const parameter = lua.createIdentifier("t");
  return lua.createVariableDeclarationStatement(
    lua.createIdentifier(GUARD),
    lua.createFunctionExpression(
      lua.createBlock([lua.createReturnStatement([parameter])]),
      [parameter],
    ),
  );
}

function entryCall() {
  return lua.createReturnStatement([
    lua.createCallExpression(lua.createIdentifier(ENTRY), []),
  ]);
}

class PiccoloPrinter extends tstl.LuaPrinter {
  printCallExpression(expression) {
    // Wrapping the passthrough itself would recurse forever.
    if (lua.isIdentifier(expression.expression) && expression.expression.text === GUARD) {
      return super.printCallExpression(expression);
    }
    return super.printCallExpression({ ...expression, params: guardTrailingTable(expression.params) });
  }

  printMethodCallExpression(expression) {
    return super.printMethodCallExpression({
      ...expression,
      params: guardTrailingTable(expression.params),
    });
  }

  printFile(file) {
    return super.printFile({
      ...file,
      statements: [guardDeclaration(), ...file.statements, ...(this.bound ? [] : [entryCall()])],
    });
  }
}

function acceptsScriptAnswer(type) {
  if (type.isUnion()) return type.types.every(acceptsScriptAnswer);
  return (type.flags & (ts.TypeFlags.NumberLike | ts.TypeFlags.Void | ts.TypeFlags.Undefined | ts.TypeFlags.Never)) !== 0;
}

function requireEntryPoint(program) {
  const diagnostics = [];
  const checker = program.getTypeChecker();
  for (const file of program.getSourceFiles()) {
    if (file.isDeclarationFile) continue;
    const declarations = file.statements.filter(
      (statement) =>
        ts.isFunctionDeclaration(statement) && statement.name && statement.name.text === ENTRY,
    );
    const implementations = declarations.filter((declaration) => declaration.body !== undefined);
    if (implementations.length !== 1) {
      diagnostics.push({
        file,
        start: declarations[0]?.getStart(file) ?? 0,
        length: declarations[0]?.getWidth(file) ?? 0,
        category: ts.DiagnosticCategory.Error,
        code: 0,
        messageText:
          `a Runtime Script declares exactly one concrete entry point as ` +
          `\`function ${ENTRY}(): number | void\`. ` +
          `The emitted Lua ends with \`return ${ENTRY}()\`, because TypeScript has no top-level ` +
          "return and a Script Answer is the chunk's return value.",
      });
      continue;
    }

    const declaration = implementations[0];
    const signature = checker.getSignatureFromDeclaration(declaration);
    const returnType = signature && checker.getReturnTypeOfSignature(signature);
    if (declaration.parameters.length === 0 && returnType && acceptsScriptAnswer(returnType)) {
      continue;
    }
    diagnostics.push({
      file,
      start: declaration.getStart(file),
      length: declaration.getWidth(file),
      category: ts.DiagnosticCategory.Error,
      code: 0,
      messageText:
        `\`${ENTRY}\` must take no parameters and return only a number or nothing. ` +
        "The Runtime Script Host calls it without arguments and accepts only a numeric Script Answer.",
    });
  }
  return diagnostics;
}

module.exports = ({ bound = false }) => ({
  beforeTransform: (program) => bound ? [] : requireEntryPoint(program),
  printer: (program, emitHost, fileName, file) => {
    const printer = new PiccoloPrinter(emitHost, program, fileName);
    printer.bound = bound;
    return printer.print(file);
  },
});

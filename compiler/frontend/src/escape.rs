//! Which variables a closure must *share* rather than copy.
//!
//! `Op::Closure` copies each captured value into the closure. That is right for a variable
//! nobody assigns, and wrong for one anybody does: JavaScript captures the **binding**, so
//!
//! ```js
//! let n = 0; let f = function () { n = 1; }; f(); n; // 1, not 0
//! ```
//!
//! A variable that is both captured and assigned therefore cannot live in a slot. It lives in
//! a heap cell, and the closure and the enclosing scope hold the same cell — so a write through
//! either is seen by both. This pass decides which variables those are.
//!
//! # Why it runs before lowering rather than during it
//!
//! The lowering discovers a capture *when it happens*: a name is captured exactly when
//! resolving it walks out of the current scope. By then the enclosing function's code has
//! already been emitted, and whether a variable is a cell changes every read and write of it.
//! So the question has to be answered first, over the whole program.
//!
//! # It over-approximates, on purpose
//!
//! The test is by **name**, not by binding: a name assigned anywhere and mentioned inside any
//! function anywhere is boxed. Shadowing makes that too eager — `let n = 1; function f() { let
//! n = 2; n = 3; }` boxes both `n`s although neither is shared.
//!
//! Being too eager costs a heap cell and an indirection. Being too clever costs correctness,
//! and the failure is a program that silently reads a stale value. Real scope resolution is
//! worth having later; guessing at it is not.

use std::collections::HashSet;

use oxc_ast::ast::{
    ArrowFunctionExpression, AssignmentTarget, IdentifierReference, Program,
    SimpleAssignmentTarget, UpdateExpression,
};
use oxc_ast_visit::Visit;

/// Names that must be shared through a cell rather than copied.
#[must_use]
pub fn shared_variables(program: &Program<'_>) -> HashSet<String> {
    let mut pass = Escape::default();
    pass.visit_program(program);
    pass.assigned
        .intersection(&pass.mentioned_inside_a_function)
        .cloned()
        .collect()
}

#[derive(Default)]
struct Escape {
    /// Names something writes to.
    assigned: HashSet<String>,
    /// Names mentioned anywhere inside a function body.
    mentioned_inside_a_function: HashSet<String>,
    /// How many function bodies deep the walk currently is.
    depth: u32,
}

impl Escape {
    fn note_assignment(&mut self, target: &AssignmentTarget<'_>) {
        // Only a plain `x = …` binds a variable. `o.x = …` and `a[i] = …` write through a
        // reference the variable already holds, which needs no sharing.
        if let AssignmentTarget::AssignmentTargetIdentifier(identifier) = target {
            self.assigned.insert(identifier.name.to_string());
        } else if let Some(SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier)) =
            target.as_simple_assignment_target()
        {
            self.assigned.insert(identifier.name.to_string());
        }
    }
}

impl<'a> Visit<'a> for Escape {
    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        if self.depth > 0 {
            self.mentioned_inside_a_function
                .insert(identifier.name.to_string());
        }
    }

    fn visit_assignment_expression(&mut self, assignment: &oxc_ast::ast::AssignmentExpression<'a>) {
        self.note_assignment(&assignment.left);
        oxc_ast_visit::walk::walk_assignment_expression(self, assignment);
    }

    /// **A `var` with an initialiser is an assignment**, because the binding already exists —
    /// hoisting created it, and the declaration only writes to it.
    ///
    /// Without this a function declared above a `var` captured the slot's value at the moment
    /// the closure was made, which is the `undefined` the hoist put there. The function was
    /// then permanently blind to the value the declaration assigned a line later, which is the
    /// shape of `function read() { return x; } var x = 7;`.
    ///
    /// `let` and `const` do not need it: their binding *is* the declaration, so nothing can
    /// capture them before it runs.
    fn visit_variable_declaration(&mut self, declaration: &oxc_ast::ast::VariableDeclaration<'a>) {
        if declaration.kind.is_var() {
            for declarator in &declaration.declarations {
                if declarator.init.is_some()
                    && let Some(name) = declarator.id.get_identifier_name()
                {
                    self.assigned.insert(name.to_string());
                }
            }
        }
        oxc_ast_visit::walk::walk_variable_declaration(self, declaration);
    }

    fn visit_function(
        &mut self,
        function: &oxc_ast::ast::Function<'a>,
        flags: oxc_syntax::scope::ScopeFlags,
    ) {
        // **A function's own name is a binding something writes to.** It is bound after the
        // closure is built, so a closure that refers to itself — `function f() { … f() … }` —
        // captures whatever the slot held at creation, which is nothing. Counting the
        // declaration as an assignment is what gives the name a cell, and a cell is shared.
        if let Some(id) = &function.id {
            self.assigned.insert(id.name.to_string());
        }
        oxc_ast_visit::walk::walk_function(self, function, flags);
    }

    fn visit_update_expression(&mut self, update: &UpdateExpression<'a>) {
        // `n++` is an assignment, and forgetting it would leave the most common mutation of a
        // captured variable — a loop counter — silently copied.
        if let SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) = &update.argument {
            self.assigned.insert(identifier.name.to_string());
        }
        oxc_ast_visit::walk::walk_update_expression(self, update);
    }

    fn visit_function_body(&mut self, body: &oxc_ast::ast::FunctionBody<'a>) {
        // The *body*, not the whole function, because `ScopeFlags` is not reachable from this
        // crate's dependencies and the body is where "inside a function" begins anyway.
        //
        // A default parameter value — `function f(a = n) {}` — mentions `n` outside the body
        // and would be missed. That cannot reach here: the lowering has no destructuring or
        // defaults, so such a parameter is already refused as unsupported.
        self.depth += 1;
        oxc_ast_visit::walk::walk_function_body(self, body);
        self.depth -= 1;
    }

    fn visit_arrow_function_expression(&mut self, arrow: &ArrowFunctionExpression<'a>) {
        self.depth += 1;
        oxc_ast_visit::walk::walk_arrow_function_expression(self, arrow);
        self.depth -= 1;
    }
}

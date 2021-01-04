use std::collections::HashSet;

use crate::folder::{fold_operation, fold_term, Folder};
use crate::formatting::ToPolarString;
use crate::kb::Bindings;
use crate::terms::{Operation, Operator, Symbol, Term, Value};
use crate::vm::{PolarVirtualMachine, VariableState};

use super::partial::{invert_operation, FALSE, TRUE};

struct VariableSubber {
    this_var: Symbol,
}

impl VariableSubber {
    pub fn new(this_var: Symbol) -> Self {
        Self { this_var }
    }
}

impl Folder for VariableSubber {
    fn fold_variable(&mut self, v: Symbol) -> Symbol {
        if v == self.this_var {
            sym!("_this")
        } else {
            v
        }
    }
}

/// Substitute `sym!("_this")` for a variable in a partial.
pub fn sub_this(this: Symbol, term: Term) -> Term {
    eprintln!("THIS: {}; TERM: {}", this, term.to_polar());
    if term
        .value()
        .as_symbol()
        .map(|s| s == &this)
        .unwrap_or(false)
    {
        return term;
    }
    fold_term(term, &mut VariableSubber::new(this))
}

/// Turn `_this = x` into `x` when it's ground.
fn simplify_trivial_constraint(this: Symbol, term: Term) -> Term {
    match term.value() {
        Value::Expression(o) if o.operator == Operator::Unify => {
            let left = &o.args[0];
            let right = &o.args[1];
            match (left.value(), right.value()) {
                (Value::Variable(v), Value::Variable(w)) if v == &this && w == &this => {
                    TRUE.into_term()
                }
                (Value::Variable(l), _) if l == &this && right.is_ground() => right.clone(),
                (_, Value::Variable(r)) if r == &this && left.is_ground() => left.clone(),
                _ => term,
            }
        }
        _ => term,
    }
}

pub fn simplify_partial(var: &Symbol, term: Term, vm: &PolarVirtualMachine) -> Term {
    let mut simplifier = Simplifier::new(var.clone(), vm);
    let simplified = simplifier.simplify_partial(term);
    let simplified = simplify_trivial_constraint(var.clone(), simplified);
    if matches!(simplified.value(), Value::Expression(e) if e.operator != Operator::And) {
        op!(And, simplified).into_term()
    } else {
        simplified
    }
}

/// Simplify the values of the bindings to be returned to the host language.
///
/// - For partials, simplify the constraint expressions.
/// - For non-partials, deep deref.
/// TODO(ap): deep deref.
pub fn simplify_bindings(bindings: Bindings, vm: &PolarVirtualMachine) -> Option<Bindings> {
    let mut unsatisfiable = false;
    let mut simplify = |var: Symbol, term: Term| {
        let simplified = simplify_partial(&var, term, vm);
        let simplified = sub_this(var, simplified);
        match simplified.value().as_expression() {
            Ok(o) if o == &FALSE => unsatisfiable = true,
            _ => (),
        }
        simplified
    };

    let bindings: Bindings = bindings
        .iter()
        // Filter out temp vars...
        .filter(|(var, _)| !var.is_temporary_var())
        .map(|(var, value)| match value.value() {
            Value::Expression(o) => {
                assert_eq!(o.operator, Operator::And);
                (var.clone(), simplify(var.clone(), value.clone()))
            }
            // ...but if a non-temp var is bound to a temp var, look through the temp var and
            // simplify the value to which it's bound.
            Value::Variable(v) if v.is_temporary_var() => {
                (var.clone(), simplify(var.clone(), bindings[v].clone()))
            }
            _ => (var.clone(), value.clone()),
        })
        .collect();

    if unsatisfiable {
        None
    } else {
        Some(bindings)
    }
}

pub struct Simplifier<'vm> {
    bindings: Bindings,
    this_var: Symbol,
    vm: &'vm PolarVirtualMachine,
}

impl<'vm> Folder for Simplifier<'vm> {
    fn fold_term(&mut self, t: Term) -> Term {
        fold_term(self.deref(&t), self)
    }

    fn fold_operation(&mut self, mut o: Operation) -> Operation {
        if o.operator == Operator::And {
            // Preprocess constraints.
            let mut seen: HashSet<&Operation> = HashSet::new();
            o = o.clone_with_constraints(
                o.constraints()
                    .iter()
                    .filter(|o| *o != &TRUE) // Drop empty constraints.
                    .filter(|o| seen.insert(o)) // Deduplicate constraints.
                    .cloned()
                    .collect(),
            );
        }

        if o.operator == Operator::And || o.operator == Operator::Or {
            // Toss trivial unifications.
            for (i, c) in o.constraints().into_iter().enumerate() {
                match c.operator {
                    Operator::Unify | Operator::Eq | Operator::Neq => {
                        assert_eq!(c.args.len(), 2);
                        let left = &c.args[0];
                        let right = &c.args[1];
                        if left == right {
                            eprintln!("TOSSING CONSTRAINT `{}`", o.args[i].to_polar());
                            o.args.remove(i);
                        }
                    }
                    _ => (),
                }
            }
        }

        match o.operator {
            // Zero-argument conjunctions & disjunctions represent constants
            // TRUE and FALSE, respectively. We do not simplify them.
            Operator::And | Operator::Or if o.args.is_empty() => o,

            // Replace one-argument conjunctions & disjunctions with their argument.
            Operator::And | Operator::Or if o.args.len() == 1 => fold_operation(
                o.args[0]
                    .value()
                    .as_expression()
                    .expect("expression")
                    .clone(),
                self,
            ),

            // Non-trivial conjunctions. Choose an (anti-)unification constraint
            // to make a binding from, maybe throw it away, and fold the rest.
            Operator::And if o.args.len() > 1 => {
                if let Some(i) = o.constraints().iter().position(|o| match o.operator {
                    // A conjunction of TRUE with X is X, so drop TRUE.
                    Operator::And if o.args.is_empty() => true,

                    // Choose an (anti)unification to maybe drop.
                    Operator::Unify | Operator::Eq | Operator::Neq => {
                        let left = &o.args[0];
                        let right = &o.args[1];
                        let invert = o.operator == Operator::Neq;
                        left == right
                            || match (left.value(), right.value()) {
                                (Value::Variable(l), _) | (Value::RestVariable(l), _)
                                    if self.is_this_var(right) =>
                                {
                                    self.bind(l.clone(), right.clone(), invert);
                                    true
                                }
                                (_, Value::Variable(r)) | (_, Value::RestVariable(r))
                                    if self.is_this_var(left) =>
                                {
                                    self.bind(r.clone(), left.clone(), invert);
                                    true
                                }
                                _ if self.is_this_var(left) || self.is_this_var(right) => false,
                                (Value::Variable(l), Value::Variable(r))
                                | (Value::Variable(l), Value::RestVariable(r))
                                | (Value::RestVariable(l), Value::Variable(r))
                                | (Value::RestVariable(l), Value::RestVariable(r)) => {
                                    match (self.vm.variable_state(l), self.vm.variable_state(r)) {
                                        (VariableState::Unbound, VariableState::Unbound) => todo!(),
                                        (VariableState::Unbound, VariableState::Cycle(_)) => {
                                            todo!()
                                        }
                                        (VariableState::Unbound, VariableState::Partial(_)) => {
                                            todo!()
                                        }
                                        (VariableState::Unbound, VariableState::Bound(_)) => {
                                            todo!()
                                        }
                                        (VariableState::Cycle(_), VariableState::Unbound) => {
                                            todo!()
                                        }
                                        (VariableState::Cycle(_), VariableState::Cycle(_)) => {
                                            todo!()
                                        }
                                        (VariableState::Cycle(_), VariableState::Partial(_)) => {
                                            todo!()
                                        }
                                        (VariableState::Cycle(_), VariableState::Bound(_)) => {
                                            todo!()
                                        }
                                        (VariableState::Partial(_), VariableState::Unbound) => {
                                            todo!()
                                        }
                                        (VariableState::Partial(_), VariableState::Cycle(_)) => {
                                            todo!()
                                        }
                                        (VariableState::Partial(_), VariableState::Partial(_)) => {
                                            if !self.is_bound(l) {
                                                self.bind(l.clone(), right.clone(), invert);
                                            }
                                            if !self.is_bound(r) {
                                                self.bind(r.clone(), left.clone(), invert);
                                            }
                                            true
                                        }
                                        (VariableState::Partial(_), VariableState::Bound(_)) => {
                                            todo!()
                                        }
                                        (VariableState::Bound(_), VariableState::Unbound) => {
                                            todo!()
                                        }
                                        (VariableState::Bound(_), VariableState::Cycle(_)) => {
                                            todo!()
                                        }
                                        (VariableState::Bound(_), VariableState::Partial(_)) => {
                                            todo!()
                                        }
                                        (VariableState::Bound(_), VariableState::Bound(_)) => {
                                            todo!()
                                        }
                                    }
                                }
                                (Value::Variable(l), _) | (Value::RestVariable(l), _) => {
                                    match self.vm.variable_state(l) {
                                        VariableState::Unbound => todo!(),
                                        VariableState::Cycle(_) => todo!(),
                                        VariableState::Partial(_) => {
                                            self.bind(l.clone(), right.clone(), invert);
                                            true
                                        }
                                        VariableState::Bound(_) => todo!(),
                                    }
                                }
                                (_, Value::Variable(r)) | (_, Value::RestVariable(r)) => {
                                    match self.vm.variable_state(r) {
                                        VariableState::Unbound => todo!(),
                                        VariableState::Cycle(_) => todo!(),
                                        VariableState::Partial(_) => {
                                            self.bind(r.clone(), left.clone(), invert);
                                            true
                                        }
                                        VariableState::Bound(_) => todo!(),
                                    }
                                }
                                // (Value::Variable(v), x)
                                //     // if !self.is_this_var(left)
                                //     //     && !self.is_bound(v)
                                //     //     && x.is_ground() =>
                                // {
                                //     eprintln!("A {} ← {}", left.to_polar(), right.to_polar());
                                //     self.bind(v.clone(), right.clone(), invert);
                                //     true
                                // }
                                // (x, Value::Variable(v))
                                //     if !self.is_this_var(right)
                                //         && !self.is_bound(v)
                                //         && x.is_ground() =>
                                // {
                                //     eprintln!("B {} ← {}", right.to_polar(), left.to_polar());
                                //     self.bind(v.clone(), left.clone(), invert);
                                //     true
                                // }
                                _ => false,
                            }
                    }
                    _ => false,
                }) {
                    eprintln!("CHOSEN CONSTRAINT: {}", &o.args[i].to_polar());
                    o.args.remove(i);
                }
                fold_operation(o, self)
            }

            // (Negated) comparisons.
            Operator::Eq
            | Operator::Neq
            | Operator::Gt
            | Operator::Geq
            | Operator::Lt
            | Operator::Leq => {
                let left = &o.args[0];
                let right = &o.args[1];
                match (left.value().as_expression(), right.value().as_expression()) {
                    (Ok(left), Ok(right))
                        if left.operator == Operator::Not && right.operator == Operator::Not =>
                    {
                        todo!("not x = not y");
                    }
                    (Ok(left), _) if left.operator == Operator::Not => {
                        invert_operation(Operation {
                            operator: o.operator,
                            args: vec![left.args[0].clone(), right.clone()],
                        })
                    }
                    (_, Ok(right)) if right.operator == Operator::Not => {
                        invert_operation(Operation {
                            operator: o.operator,
                            args: vec![left.clone(), right.args[0].clone()],
                        })
                    }
                    _ => fold_operation(o, self),
                }
            }

            // Negation.
            Operator::Not => {
                // Simplify the negated expression to eliminate temps.
                match self.simplify_partial(o.args[0].clone()).value() {
                    Value::Expression(e) => fold_operation(invert_operation(e.clone()), self),
                    _ => fold_operation(o, self),
                }
            }
            _ => fold_operation(o, self),
        }
    }
}

impl<'vm> Simplifier<'vm> {
    pub fn new(this_var: Symbol, vm: &'vm PolarVirtualMachine) -> Self {
        Self {
            this_var,
            bindings: Bindings::new(),
            vm,
        }
    }

    pub fn bind(&mut self, var: Symbol, value: Term, invert: bool) {
        let value = self.deref(&value);
        self.bindings.insert(
            var,
            if invert {
                value.clone_with_value(value!(op!(Not, value.clone())))
            } else {
                value
            },
        );
    }

    pub fn deref(&self, term: &Term) -> Term {
        match term.value() {
            Value::Variable(var) | Value::RestVariable(var) => {
                self.bindings.get(var).unwrap_or(term).clone()
            }
            _ => term.clone(),
        }
    }

    fn is_bound(&self, var: &Symbol) -> bool {
        self.bindings.contains_key(var)
    }

    fn is_this_var(&self, t: &Term) -> bool {
        matches!(t.value(), Value::Variable(v) if v == &self.this_var)
    }

    /// Simplify a partial until quiescence.
    pub fn simplify_partial(&mut self, mut term: Term) -> Term {
        let mut new;
        loop {
            eprintln!("SIMPLIFYING {}: {}", self.this_var, term.to_polar());
            new = self.fold_term(term.clone());
            eprintln!(" ⇒ {}", new.to_polar());
            if new == term {
                break;
            }
            term = new;
            self.bindings.clear();
        }
        new
    }
}

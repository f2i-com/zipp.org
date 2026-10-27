//! Classes: member modifiers, index signatures, `implements`, and the
//! members that have no run-time meaning.
//!
//! A member is read by the JavaScript class parser once TypeScript's
//! modifiers (`public`, `private`, `protected`, `readonly`, `override`,
//! `abstract`, `declare`, and `static` in any position among them) are past.
//! `declare` and `abstract` members, overload signatures and index
//! signatures are dropped: tsc emits nothing for them, and a `declare`d
//! field must not define the property (it would shadow the base class's).

use crate::parse::ast::*;
use crate::parse::parser::{PResult, Parser};
use crate::parse::token::Punct;

impl<'s> Parser<'s> {
    /// A class member, or `None` for one that is only a type. A constructor's
    /// parameter properties add their fields to `fields`, which come first in
    /// the class (tsc's `useDefineForClassFields` output declares them so).
    pub(crate) fn ts_class_member(
        &mut self,
        derived: bool,
        fields: &mut Vec<ClassMember>,
    ) -> PResult<Option<ClassMember>> {
        let dec_start = self.cur().span.start;
        let decorators = self.parse_decorators()?;
        let mut erase = false;
        while let Some(word) = self.ts_word() {
            let is_static = word == "static";
            if !is_static
                && !matches!(
                    word,
                    "public"
                        | "private"
                        | "protected"
                        | "readonly"
                        | "override"
                        | "abstract"
                        | "declare"
                )
            {
                break;
            }
            let erases = matches!(word, "abstract" | "declare");
            // `static {` is a static block, and a word followed by what cannot
            // follow a modifier is the member's own name (`readonly: boolean`,
            // `private() {}`).
            let modifier = self
                .ts_peek(|p| p.ts_can_follow_modifier() && !(is_static && p.at(Punct::LBrace)))?;
            if !modifier {
                break;
            }
            erase |= erases;
            self.bump(false)?;
            if is_static {
                self.ts_state.member_static = true;
            }
        }
        if self.at(Punct::LBracket) && self.ts_at_index_signature()? {
            self.ts_state.member_static = false;
            self.ts_index_signature()?;
            self.semicolon()?;
            return Ok(None);
        }
        let start = self.cur().span.start;
        let mut m = self.parse_class_member_inner(derived, start)?;
        self.ts_state.member_static = false;
        let props = std::mem::take(&mut self.ts_state.ctor_props);
        if matches!(&m, ClassMember::Method(cm) if cm.kind == MethodKind::Constructor) {
            fields.extend(props.into_iter().map(|name| {
                ClassMember::Field(ClassField {
                    key: PropKey::Ident(name),
                    value: None,
                    is_static: false,
                    accessor: None,
                    decorators: Vec::new(),
                })
            }));
        }
        if std::mem::take(&mut self.ts_state.bodiless) || erase {
            return Ok(None);
        }
        self.attach_member_decorators(&mut m, decorators, dec_start)?;
        Ok(Some(m))
    }

    /// After a class member's name: `?` (optional) or `!` (definitely
    /// assigned), a method's type parameters, a field's type.
    pub(crate) fn ts_member_key_suffix(&mut self) -> PResult<()> {
        if self.at(Punct::Question) || (self.at(Punct::Bang) && !self.cur().newline_before) {
            self.bump(false)?;
        }
        if self.at(Punct::Lt) {
            self.ts_type_params()?;
        } else {
            self.ts_annotation_opt()?;
        }
        Ok(())
    }

    /// Between a class's heritage and its body: the type arguments of
    /// `extends Base<T>` and the `implements` list.
    pub(crate) fn ts_class_heritage_tail(&mut self) -> PResult<()> {
        if self.at(Punct::Lt) {
            self.ts_type_args()?;
        }
        if self.ts_at("implements") {
            self.bump(false)?;
            loop {
                self.ts_type()?;
                if !self.eat(Punct::Comma, false)? {
                    break;
                }
            }
        }
        Ok(())
    }
}

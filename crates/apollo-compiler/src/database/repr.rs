use super::InputDatabase;
use crate::ast;
use crate::diagnostics::DiagnosticData;
use crate::diagnostics::Label;
use crate::schema::Name;
use crate::ApolloDiagnostic;
use crate::Arc;
use crate::FileId;
use std::collections::HashMap;
use std::collections::HashSet;

/// Queries for parsing into the various in-memory representations of GraphQL documents
#[salsa::query_group(ReprStorage)]
pub trait ReprDatabase: InputDatabase {
    /// Get a CST for a particular file. Returns a `rowan` SyntaxTree. The
    /// SyntaxTree can be safely shared between threads as it's `Send` and
    /// `Sync`.
    #[salsa::invoke(cst)]
    fn cst(&self, file_id: FileId) -> apollo_parser::SyntaxTree;

    #[salsa::invoke(ast_parse_result)]
    #[doc(hidden)]
    fn _ast_parse_result(&self, file_id: FileId) -> Arc<ParseResult>;

    #[salsa::invoke(ast)]
    #[salsa::transparent]
    fn ast(&self, file_id: FileId) -> Arc<ast::Document>;

    #[salsa::invoke(syntax_errors)]
    #[salsa::transparent]
    fn syntax_errors(&self, file_id: FileId) -> Arc<Vec<ApolloDiagnostic>>;

    #[salsa::invoke(recursion_reached)]
    #[salsa::transparent]
    fn recursion_reached(&self, file_id: FileId) -> usize;

    #[salsa::invoke(tokens_reached)]
    #[salsa::transparent]
    fn tokens_reached(&self, file_id: FileId) -> usize;

    #[salsa::invoke(schema)]
    fn schema(&self) -> Arc<crate::Schema>;

    #[salsa::invoke(executable_document)]
    fn executable_document(
        &self,
        file_id: FileId,
    ) -> Result<Arc<crate::ExecutableDocument>, crate::executable::TypeError>;

    // TODO: another database trait? what to name it?

    /// Returns a map of interface names to names of types that implement that interface
    ///
    /// `Schema` only stores the inverse relationship
    /// (in [`ObjectType::implements_interfaces`] and [`InterfaceType::implements_interfaces`]),
    /// so finding the implementers of even one interface requires a linear scan.
    /// Gathering them all at once and caching amorticizes that cost.
    #[salsa::invoke(implementers_map)]
    fn implementers_map(&self) -> Arc<HashMap<Name, HashSet<Name>>>;

    /// Returns whether `maybe_subtype` is a subtype of `abstract_type`, which means either:
    ///
    /// * `maybe_subtype` implements the interface `abstract_type`
    /// * `maybe_subtype` is a member of the union type `abstract_type`
    #[salsa::invoke(is_subtype)]
    #[salsa::transparent]
    fn is_subtype(&self, abstract_type: &str, maybe_subtype: &str) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseResult {
    document: Arc<ast::Document>,
    syntax_errors: Arc<Vec<ApolloDiagnostic>>,
    recursion_reached: usize,
    tokens_reached: usize,
}

fn cst(db: &dyn ReprDatabase, file_id: FileId) -> apollo_parser::SyntaxTree {
    let input = db.source_code(file_id);
    let mut parser = apollo_parser::Parser::new(&input);
    if let Some(limit) = db.recursion_limit() {
        parser = parser.recursion_limit(limit);
    }
    if let Some(limit) = db.token_limit() {
        parser = parser.token_limit(limit);
    }
    parser.parse()
}

fn ast_parse_result(db: &dyn ReprDatabase, file_id: FileId) -> Arc<ParseResult> {
    let tree = db.cst(file_id);
    let syntax_errors = tree.errors();
    let recursion_reached = tree.recursion_limit().high;
    let tokens_reached = tree.token_limit().high;
    let document = Arc::new(ast::Document::from_cst(tree.document(), file_id));
    Arc::new(ParseResult {
        document,
        recursion_reached,
        tokens_reached,
        syntax_errors: Arc::new(
            syntax_errors
                .map(|err| {
                    if err.is_limit() {
                        ApolloDiagnostic::new(
                            db,
                            (file_id, err.index(), err.data().len()).into(),
                            DiagnosticData::LimitExceeded {
                                message: err.message().into(),
                            },
                        )
                        .label(Label::new(
                            (file_id, err.index(), err.data().len()),
                            err.message(),
                        ))
                    } else {
                        ApolloDiagnostic::new(
                            db,
                            (file_id, err.index(), err.data().len()).into(),
                            DiagnosticData::SyntaxError {
                                message: err.message().into(),
                            },
                        )
                        .label(Label::new(
                            (file_id, err.index(), err.data().len()),
                            err.message(),
                        ))
                    }
                })
                .collect(),
        ),
    })
}

fn ast(db: &dyn ReprDatabase, file_id: FileId) -> Arc<ast::Document> {
    db._ast_parse_result(file_id).document.clone()
}

fn syntax_errors(db: &dyn ReprDatabase, file_id: FileId) -> Arc<Vec<ApolloDiagnostic>> {
    db._ast_parse_result(file_id).syntax_errors.clone()
}

fn recursion_reached(db: &dyn ReprDatabase, file_id: FileId) -> usize {
    db._ast_parse_result(file_id).recursion_reached
}

fn tokens_reached(db: &dyn ReprDatabase, file_id: FileId) -> usize {
    db._ast_parse_result(file_id).tokens_reached
}

fn schema(db: &dyn ReprDatabase) -> Arc<crate::Schema> {
    let mut builder = crate::Schema::builder();
    for file_id in db.type_definition_files() {
        builder.add_document(&db.ast(file_id))
    }
    let (schema, _orphan_extensions) = builder.build();
    Arc::new(schema)
}

fn executable_document(
    db: &dyn ReprDatabase,
    file_id: FileId,
) -> Result<Arc<crate::ExecutableDocument>, crate::executable::TypeError> {
    crate::ExecutableDocument::from_ast(&db.schema(), &db.ast(file_id)).map(Arc::new)
}

fn implementers_map(db: &dyn ReprDatabase) -> Arc<HashMap<Name, HashSet<Name>>> {
    Arc::new(db.schema().implementers_map())
}

fn is_subtype(db: &dyn ReprDatabase, abstract_type: &str, maybe_subtype: &str) -> bool {
    db.schema()
        .is_subtype(&db.implementers_map(), abstract_type, maybe_subtype)
}
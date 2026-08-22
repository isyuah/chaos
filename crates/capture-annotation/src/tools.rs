//! Annotation tools a user can select in the editor.

/// A tool the user can draw with. UI-neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnotationTool {
    Pen,
    Rectangle,
}

impl AnnotationTool {
    pub const ALL: [AnnotationTool; 2] = [AnnotationTool::Pen, AnnotationTool::Rectangle];

    pub const fn id(self) -> &'static str {
        match self {
            AnnotationTool::Pen => "pen",
            AnnotationTool::Rectangle => "rectangle",
        }
    }
}

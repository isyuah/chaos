//! Annotation tools a user can select in the editor.

/// A tool the user can draw with. UI-neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnotationTool {
    /// Select, move, and resize the current capture.
    Pointer,
    Pen,
    Rectangle,
}

impl AnnotationTool {
    pub const ALL: [AnnotationTool; 3] = [
        AnnotationTool::Pointer,
        AnnotationTool::Pen,
        AnnotationTool::Rectangle,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            AnnotationTool::Pointer => "pointer",
            AnnotationTool::Pen => "pen",
            AnnotationTool::Rectangle => "rectangle",
        }
    }
}

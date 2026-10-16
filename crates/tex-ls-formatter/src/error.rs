// Keep distinct public enums so existing enum-variant imports remain valid.
macro_rules! define_format_error {
    ($(#[$meta:meta])* $kind:ty) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum FormatError {
            /// The parser reported syntax errors.
            ParseErrors { count: usize },
            /// Recovery tokens or uncertain ownership prevent formatting.
            UnsupportedConstruct { kind: $kind, snippet: String },
        }

        impl std::fmt::Display for FormatError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::ParseErrors { count } => write!(
                        f,
                        "input contains {count} parser diagnostic(s); formatter only supports parseable input"
                    ),
                    Self::UnsupportedConstruct { kind, snippet } => write!(
                        f,
                        "unsupported construct for formatter: {kind:?} near {snippet:?}"
                    ),
                }
            }
        }

        impl std::error::Error for FormatError {}
    };
}

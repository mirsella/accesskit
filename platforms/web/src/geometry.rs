use accesskit::Rect;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CssRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

pub(crate) fn map_rect(bounds: Rect, logical: Rect, canvas: CssRect) -> Option<CssRect> {
    let logical_width = logical.x1 - logical.x0;
    let logical_height = logical.y1 - logical.y0;
    if logical_width <= 0.0 || logical_height <= 0.0 || canvas.width <= 0.0 || canvas.height <= 0.0
    {
        return None;
    }

    let scale_x = canvas.width / logical_width;
    let scale_y = canvas.height / logical_height;
    let width = (bounds.x1 - bounds.x0) * scale_x;
    let height = (bounds.y1 - bounds.y0) * scale_y;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    Some(CssRect {
        left: canvas.left + (bounds.x0 - logical.x0) * scale_x,
        top: canvas.top + (bounds.y0 - logical.y0) * scale_y,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_logical_bounds_to_canvas_css_pixels() {
        let actual = map_rect(
            Rect::new(100.0, 50.0, 300.0, 150.0),
            Rect::new(0.0, 0.0, 800.0, 400.0),
            CssRect {
                left: 10.0,
                top: 20.0,
                width: 400.0,
                height: 200.0,
            },
        );
        assert_eq!(
            actual,
            Some(CssRect {
                left: 60.0,
                top: 45.0,
                width: 100.0,
                height: 50.0,
            })
        );
    }

    #[test]
    fn rejects_empty_geometry() {
        assert_eq!(
            map_rect(
                Rect::new(0.0, 0.0, 0.0, 10.0),
                Rect::new(0.0, 0.0, 100.0, 100.0),
                CssRect {
                    left: 0.0,
                    top: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
            ),
            None
        );
    }
}

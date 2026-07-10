const std = @import("std");

const Shape = union(enum) {
    circle: f64,
    rect: struct { w: f64, h: f64 },

    fn area(self: Shape) f64 {
        return switch (self) {
            .circle => |r| 3.14159 * r * r,
            .rect => |dims| dims.w * dims.h,
        };
    }
};

pub fn main() void {
    const shapes = [_]Shape{
        .{ .circle = 2.0 },
        .{ .rect = .{ .w = 3.0, .h = 4.0 } },
    };

    var total: f64 = 0;
    for (shapes) |s| {
        total += s.area();
    }

    var i: usize = 0;
    while (i < 3) : (i += 1) {
        if (i % 2 == 0) {
            std.debug.print("even {d}\n", .{i});
        } else {
            std.debug.print("odd {d}\n", .{i});
        }
    }

    std.debug.print("total={d}\n", .{total});
}

import Foundation

struct Point {
  let x: Int
  let y: Int
}

enum Shape {
  case circle(radius: Double)
  case rect(w: Double, h: Double)
}

func area(of shape: Shape) -> Double {
  switch shape {
    case .circle(let radius):
  return 3.14159 * radius * radius
    case .rect(let w, let h):
  return w * h
  }
}

func main() {
  let points = [Point(x: 1, y: 2), Point(x: 3, y: 4)]
  let sum = points.map { p in
    p.x + p.y
  }.reduce(0) { acc, v in
    acc + v
  }
  print("sum=\(sum)")

  let banner = """
    hello
      world
    """
  print(banner)

  for i in 0..<3 {
    if i % 2 == 0 {
      print("even \(i)")
    } else {
      print("odd \(i)")
    }
  }
}

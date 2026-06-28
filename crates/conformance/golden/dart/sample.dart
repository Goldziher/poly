// Exercises nested blocks, switch, closures, and a class.
class Greeter {
  final String name;
  Greeter(this.name);

  String greet() {
    return 'hello $name';
  }
}

int classify(int n) {
  switch (n) {
    case 0:
      return 0;
    case 1:
    case 2:
      return 1;
    default:
      return -1;
  }
}

void main() {
  final names = ['ada', 'alan', 'grace'];
  final greetings = names.map((n) {
    return Greeter(n).greet();
  }).toList();

  for (final g in greetings) {
    print(g);
  }

  var total = 0;
  for (var i = 0; i < 5; i++) {
    if (i.isEven) {
      total += i;
    }
  }
  print('total=$total classify=${classify(total)}');
}
Formatted 1 file (1 changed) in 0.00 seconds.

using System;
using System.Collections.Generic;
using System.Linq;

namespace Conformance
{
    public class Sample
    {
        public static int Classify(int n)
        {
            switch (n)
            {
                case 0:
                return 0;
                case 1:
                case 2:
                return 1;
                default:
                return -1;
            }
        }

        public static void Main(string[] args)
        {
            var numbers = new List<int> { 1, 2, 3, 4, 5 };
            var evens = numbers.Where(x => x % 2 == 0).Select(x => x * x).ToList();

            foreach (var e in evens)
            {
                if (e > 4)
                {
                    Console.WriteLine($"big {e}");
                }
                else
                {
                    Console.WriteLine($"small {e}");
                }
            }

            var total = numbers.Aggregate(0, (acc, v) => acc + v);
            Console.WriteLine($"total={total} classify={Classify(total)}");
        }
    }
}

// Runs dumpils' own demodulator on a cu8 file (1310720 S/s, channel at +200 kHz),
// the same 0.1 s blocks and µA scaling as cmd/dumpils. Benchmark use only.
package main

import (
	"fmt"
	"io"
	"os"
	"strconv"

	"github.com/asgaut/dumpils/pkg/demod"
)

func main() {
	f, err := os.Open(os.Args[1])
	if err != nil { panic(err) }
	gs := len(os.Args) > 2 && os.Args[2] == "gs"
	fs := 10.0 * float64(1<<17)
	off := 200.0e3
	if len(os.Args) > 3 { off, _ = strconv.ParseFloat(os.Args[3], 64) }
	d := demod.NewDemodulator(off, int(fs/10))
	buf := make([]byte, int(fs/10)<<1)
	fmt.Println("RF(dbFS);DDM(uA);SDM(%);Ident")
	for {
		if _, err := io.ReadFull(f, buf); err != nil { break }
		p, dd, s, i := d.Process(buf)
		if gs { dd *= 150 / 17.5 } else { dd *= 150 / 15.5 }
		fmt.Printf("%.1f;%.3f;%.3f;%.3f\n", p, dd, s, i)
	}
}

package orderbook

// MaxPriceHeap implements heap.Interface for bid prices (highest price on top)
// Use container/heap package to manipulate this heap (Init, Push, Pop, Remove)
type MaxPriceHeap []int64

func (h MaxPriceHeap) Len() int           { return len(h) }
func (h MaxPriceHeap) Less(i, j int) bool { return h[i] > h[j] } // Max heap: larger values bubble up
func (h MaxPriceHeap) Swap(i, j int)      { h[i], h[j] = h[j], h[i] }

func (h *MaxPriceHeap) Push(x interface{}) {
	*h = append(*h, x.(int64))
}

func (h *MaxPriceHeap) Pop() interface{} {
	old := *h
	n := len(old)
	x := old[n-1]
	*h = old[0 : n-1]
	return x
}

// Peek returns the top element without removing it
func (h MaxPriceHeap) Peek() int64 {
	if len(h) == 0 {
		return 0
	}
	return h[0]
}

// MinPriceHeap implements heap.Interface for ask prices (lowest price on top)
type MinPriceHeap []int64

func (h MinPriceHeap) Len() int           { return len(h) }
func (h MinPriceHeap) Less(i, j int) bool { return h[i] < h[j] } // Min heap: smaller values bubble up
func (h MinPriceHeap) Swap(i, j int)      { h[i], h[j] = h[j], h[i] }

func (h *MinPriceHeap) Push(x interface{}) {
	*h = append(*h, x.(int64))
}

func (h *MinPriceHeap) Pop() interface{} {
	old := *h
	n := len(old)
	x := old[n-1]
	*h = old[0 : n-1]
	return x
}

// Peek returns the top element without removing it
func (h MinPriceHeap) Peek() int64 {
	if len(h) == 0 {
		return 0
	}
	return h[0]
}

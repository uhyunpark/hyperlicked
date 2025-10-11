package util

import "time"

type Clock interface {
	After(d time.Duration) <-chan time.Time
	Now() time.Time
}

type RealClock struct{}

func (RealClock) After(d time.Duration) <-chan time.Time { return time.After(d) }
func (RealClock) Now() time.Time                         { return time.Now() }

package api

import (
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/gorilla/mux"
	"github.com/rs/cors"
	"github.com/uhyunpark/hyperlicked/pkg/app/perp"
)

// Server handles REST API and WebSocket connections
type Server struct {
	app    *perp.App
	router *mux.Router
	hub    *Hub // WebSocket hub
	txLog  *os.File // Transaction log file
}

// NewServer creates a new API server
func NewServer(app *perp.App) *Server {
	// Open transaction log file
	txLogPath := os.Getenv("TX_LOG_FILE")
	if txLogPath == "" {
		txLogPath = "data/transactions.log"
	}

	// Create data directory if it doesn't exist
	os.MkdirAll("data", 0755)

	txLog, err := os.OpenFile(txLogPath, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644)
	if err != nil {
		log.Printf("[api] WARNING: failed to open tx log file %s: %v", txLogPath, err)
		txLog = nil // Continue without tx logging
	} else {
		log.Printf("[api] transaction log: %s", txLogPath)
	}

	s := &Server{
		app:    app,
		router: mux.NewRouter(),
		hub:    NewHub(),
		txLog:  txLog,
	}

	s.setupRoutes()
	return s
}

func (s *Server) setupRoutes() {
	// API v1 routes
	api := s.router.PathPrefix("/api/v1").Subrouter()

	// Market endpoints
	api.HandleFunc("/markets", s.handleGetMarkets).Methods("GET")
	api.HandleFunc("/markets/{symbol}", s.handleGetMarket).Methods("GET")
	api.HandleFunc("/markets/{symbol}/orderbook", s.handleGetOrderbook).Methods("GET")
	api.HandleFunc("/markets/{symbol}/trades", s.handleGetTrades).Methods("GET")

	// Account endpoints
	api.HandleFunc("/accounts/{address}", s.handleGetAccount).Methods("GET")
	api.HandleFunc("/accounts/{address}/positions", s.handleGetPositions).Methods("GET")
	api.HandleFunc("/accounts/{address}/orders", s.handleGetOrders).Methods("GET")

	// Chain endpoints
	api.HandleFunc("/chain/status", s.handleGetChainStatus).Methods("GET")

	// Order submission
	api.HandleFunc("/orders", s.handleSubmitOrder).Methods("POST")
	api.HandleFunc("/orders/cancel", s.handleCancelOrder).Methods("POST")

	// WebSocket endpoint
	s.router.HandleFunc("/ws", s.handleWebSocket)

	// Health check
	s.router.HandleFunc("/health", s.handleHealth).Methods("GET")
}

// Start starts the API server
func (s *Server) Start(addr string) error {
	// Start WebSocket hub
	go s.hub.Run()

	// CORS configuration
	c := cors.New(cors.Options{
		AllowedOrigins: []string{"http://localhost:3000", "http://localhost:3001"},
		AllowedMethods: []string{"GET", "POST", "OPTIONS"},
		AllowedHeaders: []string{"Content-Type", "Authorization"},
		AllowCredentials: true,
	})

	handler := c.Handler(s.router)

	log.Printf("[api] server starting on %s", addr)
	return http.ListenAndServe(addr, handler)
}

// ==============================
// REST Handlers
// ==============================

func (s *Server) handleGetMarkets(w http.ResponseWriter, r *http.Request) {
	markets := s.app.ListMarkets()

	response := make([]MarketInfo, len(markets))
	for i, m := range markets {
		response[i] = MarketInfo{
			Symbol:               m.Symbol,
			BaseAsset:            m.BaseAsset,
			QuoteAsset:           m.QuoteAsset,
			Type:                 m.Type.String(),
			Status:               m.Status.String(),
			TickSize:             m.TickSize,
			LotSize:              m.LotSize,
			MaxLeverage:          int(m.MaxLeverage),
			TakerFeeBps:          m.TakerFeeBps,
			MakerFeeBps:          m.MakerFeeBps,
			MaintenanceMarginBps: m.MaintenanceMarginBps,
		}
	}

	respondJSON(w, response)
}

func (s *Server) handleGetMarket(w http.ResponseWriter, r *http.Request) {
	vars := mux.Vars(r)
	symbol := vars["symbol"]

	market, err := s.app.GetMarket(symbol)
	if err != nil {
		respondError(w, http.StatusNotFound, "market not found", err.Error())
		return
	}

	response := MarketInfo{
		Symbol:               market.Symbol,
		BaseAsset:            market.BaseAsset,
		QuoteAsset:           market.QuoteAsset,
		Type:                 market.Type.String(),
		Status:               market.Status.String(),
		TickSize:             market.TickSize,
		LotSize:              market.LotSize,
		MaxLeverage:          int(market.MaxLeverage),
		TakerFeeBps:          market.TakerFeeBps,
		MakerFeeBps:          market.MakerFeeBps,
		MaintenanceMarginBps: market.MaintenanceMarginBps,
	}

	respondJSON(w, response)
}

func (s *Server) handleGetOrderbook(w http.ResponseWriter, r *http.Request) {
	vars := mux.Vars(r)
	symbol := vars["symbol"]

	book := s.app.GetOrderbook(symbol)
	if book == nil {
		respondError(w, http.StatusNotFound, "orderbook not found", "")
		return
	}

	// Get sorted price levels
	bidLevels := book.GetBidLevels()
	askLevels := book.GetAskLevels()

	// Convert to API format
	bids := make([]PriceLevel, len(bidLevels))
	for i, level := range bidLevels {
		bids[i] = PriceLevel{Price: level.Price, Size: level.Qty}
	}

	asks := make([]PriceLevel, len(askLevels))
	for i, level := range askLevels {
		asks[i] = PriceLevel{Price: level.Price, Size: level.Qty}
	}

	response := OrderbookSnapshot{
		Symbol:    symbol,
		Bids:      bids,
		Asks:      asks,
		Timestamp: time.Now().UnixMilli(),
	}

	respondJSON(w, response)
}

func (s *Server) handleGetTrades(w http.ResponseWriter, r *http.Request) {
	// TODO: Implement trade history tracking in app layer
	// For now, return empty array
	respondJSON(w, []TradeInfo{})
}

func (s *Server) handleGetAccount(w http.ResponseWriter, r *http.Request) {
	vars := mux.Vars(r)
	addressStr := vars["address"]

	if !common.IsHexAddress(addressStr) {
		respondError(w, http.StatusBadRequest, "invalid address", "")
		return
	}

	addr := common.HexToAddress(addressStr)
	account := s.app.GetAccount(addr)

	// Calculate total equity (balance + unrealized PnL)
	// TODO: Calculate unrealized PnL from positions
	totalEquity := account.USDCBalance

	response := AccountInfo{
		Address:          addr.Hex(),
		Balance:          account.USDCBalance,
		LockedCollateral: account.LockedCollateral,
		AvailableBalance: account.USDCBalance - account.LockedCollateral,
		UnrealizedPnL:    0, // TODO: Calculate from positions
		TotalEquity:      totalEquity,
	}

	respondJSON(w, response)
}

func (s *Server) handleGetPositions(w http.ResponseWriter, r *http.Request) {
	vars := mux.Vars(r)
	addressStr := vars["address"]

	if !common.IsHexAddress(addressStr) {
		respondError(w, http.StatusBadRequest, "invalid address", "")
		return
	}

	addr := common.HexToAddress(addressStr)
	account := s.app.GetAccount(addr)

	// Convert positions to API format
	positions := make([]PositionInfo, 0, len(account.Positions))
	for symbol, pos := range account.Positions {
		if pos.Size == 0 {
			continue // Skip closed positions
		}

		// TODO: Get mark price from oracle
		markPrice := pos.EntryPrice // Use entry price as placeholder

		// Calculate unrealized PnL
		pnl := pos.UnrealizedPnL(markPrice)

		// Calculate liquidation price (TODO: implement properly)
		liquidationPrice := pos.EntryPrice * 9 / 10 // Placeholder: 10% drop

		positions = append(positions, PositionInfo{
			Symbol:           symbol,
			Size:             pos.Size,
			EntryPrice:       pos.EntryPrice,
			MarkPrice:        markPrice,
			LiquidationPrice: liquidationPrice,
			UnrealizedPnL:    pnl,
			Margin:           pos.Margin,
			Leverage:         float64(pos.Leverage(markPrice)),
		})
	}

	respondJSON(w, positions)
}

func (s *Server) handleGetOrders(w http.ResponseWriter, r *http.Request) {
	// TODO: Implement order tracking in app layer
	// For now, return empty array
	respondJSON(w, []OrderInfo{})
}

func (s *Server) handleGetChainStatus(w http.ResponseWriter, r *http.Request) {
	// TODO: Access consensus state (need to pass State reference)
	// For now, return placeholder
	response := ChainStatus{
		Height:       0,
		View:         0,
		AvgBlockTime: 100.0,
		MempoolSize:  s.app.GetMempoolSize(),
		Validators:   4,
	}

	respondJSON(w, response)
}

func (s *Server) handleSubmitOrder(w http.ResponseWriter, r *http.Request) {
	// Read signed transaction body
	bodyBytes, err := io.ReadAll(r.Body)
	if err != nil {
		respondError(w, http.StatusBadRequest, "failed to read body", err.Error())
		return
	}

	// Parse signed transaction
	var signedTx map[string]interface{}
	if err := json.Unmarshal(bodyBytes, &signedTx); err != nil {
		respondError(w, http.StatusBadRequest, "invalid JSON transaction", err.Error())
		return
	}

	// Validate transaction type
	txType, ok := signedTx["type"].(string)
	if !ok || txType != "order" {
		respondError(w, http.StatusBadRequest, "invalid transaction type", "expected type=order")
		return
	}

	// Validate signature exists
	sig, ok := signedTx["signature"].(string)
	if !ok || sig == "" {
		respondError(w, http.StatusBadRequest, "missing signature", "")
		return
	}

	// Submit JSON transaction directly to mempool
	s.app.PushTx(bodyBytes)

	// Generate order ID from signature (first 8 chars of signature)
	orderID := "0x" + sig[2:10]

	log.Printf("[api] signed order submitted: id=%s bytes=%d", orderID, len(bodyBytes))

	// Log to file with timestamp
	s.logTransaction("ORDER_SUBMIT", map[string]interface{}{
		"order_id":  orderID,
		"signature": sig,
		"tx_bytes":  len(bodyBytes),
	})

	response := SubmitOrderResponse{
		Status:  "submitted",
		OrderID: orderID,
	}

	respondJSON(w, response)
}

func (s *Server) handleCancelOrder(w http.ResponseWriter, r *http.Request) {
	var req CancelOrderRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		respondError(w, http.StatusBadRequest, "invalid request body", err.Error())
		return
	}

	// Validate request
	if req.OrderID == "" {
		respondError(w, http.StatusBadRequest, "missing orderId", "")
		return
	}

	// TODO: Verify signature (Phase 2)
	// TODO: Verify order belongs to address

	// Format cancel transaction
	tx := fmt.Sprintf("C:BTC-USDT:%s", req.OrderID)

	// Submit to mempool
	s.app.PushTx([]byte(tx))

	log.Printf("[api] cancel submitted: %s", tx)

	// Log to file
	s.logTransaction("ORDER_CANCEL", map[string]interface{}{
		"order_id": req.OrderID,
		"address":  req.Address,
		"tx":       tx,
	})

	response := map[string]string{
		"status":  "submitted",
		"orderId": req.OrderID,
	}

	respondJSON(w, response)
}

func (s *Server) handleHealth(w http.ResponseWriter, r *http.Request) {
	respondJSON(w, map[string]string{"status": "ok"})
}

// ==============================
// Broadcast Methods (called from consensus)
// ==============================

// BroadcastOrderbook broadcasts orderbook update to WebSocket clients
func (s *Server) BroadcastOrderbook(symbol string, height int64) {
	book := s.app.GetOrderbook(symbol)
	if book == nil {
		return
	}

	bidLevels := book.GetBidLevels()
	askLevels := book.GetAskLevels()

	bids := make([]PriceLevel, len(bidLevels))
	for i, level := range bidLevels {
		bids[i] = PriceLevel{Price: level.Price, Size: level.Qty}
	}

	asks := make([]PriceLevel, len(askLevels))
	for i, level := range askLevels {
		asks[i] = PriceLevel{Price: level.Price, Size: level.Qty}
	}

	update := OrderbookUpdate{
		Type:      "orderbook",
		Symbol:    symbol,
		Bids:      bids,
		Asks:      asks,
		Timestamp: time.Now().UnixMilli(),
		Height:    height,
	}

	s.hub.BroadcastToChannel("orderbook:"+symbol, update)
}

// ==============================
// Helper Functions
// ==============================

func respondJSON(w http.ResponseWriter, data interface{}) {
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(data)
}

func respondError(w http.ResponseWriter, status int, error string, message string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(ErrorResponse{
		Error:   error,
		Message: message,
	})
}

// logTransaction writes a transaction event to the log file
func (s *Server) logTransaction(eventType string, data map[string]interface{}) {
	if s.txLog == nil {
		return // Logging disabled
	}

	// Create log entry
	entry := map[string]interface{}{
		"timestamp": time.Now().Format(time.RFC3339),
		"event":     eventType,
		"data":      data,
	}

	// Convert to JSON
	jsonData, err := json.Marshal(entry)
	if err != nil {
		log.Printf("[api] failed to marshal tx log entry: %v", err)
		return
	}

	// Write to file (one JSON object per line)
	s.txLog.Write(jsonData)
	s.txLog.Write([]byte("\n"))
}
